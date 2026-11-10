// C-ABI implementation over psd_sdk (Molecular Matters, C++), compiled through the cc crate
// (MSVC on Windows, clang on macOS). The C++ STL is fine here; only wrapper.h stays C-style.
//
// ABI version 2: the merge of fire's and 3D Review's wrappers (fire-decode plan, FD3).
// - From 3D Review: the staged decode (header first, planes only once the caller's limits allow
//   them), borrowed bytes, and the checks on sections, channels and planes.
// - From fire: every colour mode, the document's own depth, and the integer, threaded
//   interleave for 8/16-bit RGB and greyscale.
// - Neither: limits and the thread count come from the caller, and psd_sdk's image-resources
//   parser is never called (the composite's ICC profile is found in Rust).
//
// Scope: the MERGED / composited image only (present when the PSD was saved with "Maximize
// Compatibility"). Layers are decoded in Rust by fire-decode's `psd_layers`, never through
// psd_sdk's layer parser.

#include "wrapper.h"

#include "Psd.h"
#include "PsdMallocAllocator.h"
#include "PsdAllocator.h"
#include "PsdFile.h"
#include "PsdDocument.h"
#include "PsdParseDocument.h"
#include "PsdColorMode.h"
#include "PsdPlanarImage.h"
#include "PsdImageDataSection.h"
#include "PsdParseImageDataSection.h"
#include "PsdColorModeDataSection.h"
#include "PsdParseColorModeDataSection.h"

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstring>
#include <new>
#include <thread>
#include <vector>

// The layout src/bindings.rs asserts on the Rust side. If wrapper.h changes, both sides change.
static_assert(sizeof(fire_psd_info) == 16, "fire_psd_info size");
static_assert(alignof(fire_psd_info) == 4, "fire_psd_info alignment");
static_assert(offsetof(fire_psd_info, channels) == 8, "fire_psd_info.channels");
static_assert(offsetof(fire_psd_info, bits_per_channel) == 10, "fire_psd_info.bits_per_channel");
static_assert(offsetof(fire_psd_info, color_mode) == 12, "fire_psd_info.color_mode");
static_assert(offsetof(fire_psd_info, has_alpha) == 14, "fire_psd_info.has_alpha");
static_assert(sizeof(fire_psd_limits) == 32, "fire_psd_limits size");
static_assert(alignof(fire_psd_limits) == 8, "fire_psd_limits alignment");
static_assert(offsetof(fire_psd_limits, max_output_bytes) == 8, "fire_psd_limits.max_output_bytes");
static_assert(offsetof(fire_psd_limits, max_planar_bytes) == 16, "fire_psd_limits.max_planar_bytes");
static_assert(offsetof(fire_psd_limits, max_channels) == 24, "fire_psd_limits.max_channels");

namespace {

// psd_sdk reads exclusively through the abstract File interface (async by contract).
// We back it with an in-memory buffer and serve reads synchronously.
class MemoryFile : public psd::File {
public:
    MemoryFile(psd::Allocator* allocator, const uint8_t* data, size_t size)
        : psd::File(allocator), m_data(data), m_size(size), m_shortRead(false) {}

    // Whether any read ran past end-of-file, i.e. the document is truncated. psd_sdk has no way
    // to tell us a parse went off the end (it does not check the ReadOperation we hand back, and
    // its offsets come from the file's own headers), so the caller checks this after parsing and
    // rejects the document. See DoRead.
    bool HadShortRead(void) const { return m_shortRead; }

private:
    bool DoOpenRead(const wchar_t*) PSD_OVERRIDE { return true; }
    bool DoOpenWrite(const wchar_t*) PSD_OVERRIDE { return false; }
    bool DoClose(void) PSD_OVERRIDE { return true; }

    ReadOperation DoRead(void* buffer, uint32_t count, uint64_t position) PSD_OVERRIDE {
        if (position > m_size) {
            m_shortRead = true;
            std::memset(buffer, 0, count);
            return nullptr;
        }
        const uint64_t available = m_size - position;
        const uint32_t toCopy = (count <= available) ? count : static_cast<uint32_t>(available);
        std::memcpy(buffer, m_data + position, toCopy);
        // A truncated file cannot be served in full. The buffer psd_sdk handed us is raw malloc'd
        // memory, so the tail we did not fill would otherwise stay *uninitialized* — and psd_sdk,
        // believing the read succeeded, would sample it straight into the composite. Zero the
        // remainder and record the truncation so the document is refused.
        if (toCopy < count) {
            std::memset(static_cast<uint8_t*>(buffer) + toCopy, 0, count - toCopy);
            m_shortRead = true;
        }
        // Non-null sentinel: the read already completed; WaitForRead just acknowledges it.
        return reinterpret_cast<ReadOperation>(1);
    }
    bool DoWaitForRead(ReadOperation&) PSD_OVERRIDE { return true; }

    WriteOperation DoWrite(const void*, uint32_t, uint64_t) PSD_OVERRIDE { return nullptr; }
    bool DoWaitForWrite(WriteOperation&) PSD_OVERRIDE { return true; }

    uint64_t DoGetSize(void) const PSD_OVERRIDE { return m_size; }

    const uint8_t* m_data;
    size_t m_size;
    bool m_shortRead;
};

// Everything parsed for one document, owned behind a single opaque handle. The file's bytes are
// the caller's, borrowed until fire_psd_free (see wrapper.h): copying them would be a second
// whole-file allocation per decode for nothing, since the Rust caller's slice outlives the handle
// by construction.
struct Doc {
    psd::MallocAllocator allocator;
    const uint8_t* bytes = nullptr;  // borrowed; MemoryFile points into this
    size_t size = 0;
    MemoryFile* file = nullptr;
    psd::Document* document = nullptr;
    psd::ImageDataSection* imageData = nullptr;
    psd::ColorModeDataSection* colorModeData = nullptr;  // INDEXED palette lives here
};

// Only these depths have a defined plane layout. 1-bit (BITMAP mode) must be refused rather than
// guessed at: psd_sdk sizes its planes with `bitsPerChannel / 8u`, which is *zero* for 1 bit, so
// every plane is a zero-byte allocation. Sampling one would read straight off the end of the heap.
inline bool bits_are_supported(unsigned int bits) {
    return bits == 8 || bits == 16 || bits == 32;
}

// The colour modes the merged read knows how to draw.
inline bool mode_is_known(unsigned int mode) {
    switch (mode) {
        case psd::colorMode::BITMAP:
        case psd::colorMode::GRAYSCALE:
        case psd::colorMode::INDEXED:
        case psd::colorMode::RGB:
        case psd::colorMode::CMYK:
        case psd::colorMode::MULTICHANNEL:
        case psd::colorMode::DUOTONE:
        case psd::colorMode::LAB:
            return true;
        default:
            return false;
    }
}

// How many of a document's channels carry COLOUR, i.e. where the alpha channel starts.
// Everything past this index is alpha (index colorChannels) then spot/extra channels.
inline unsigned int color_channel_count(unsigned int colorMode, unsigned int channelCount) {
    switch (colorMode) {
        case psd::colorMode::BITMAP:
        case psd::colorMode::GRAYSCALE:
        case psd::colorMode::INDEXED:
        case psd::colorMode::DUOTONE:
            return 1;
        case psd::colorMode::RGB:
        case psd::colorMode::LAB:
            return 3;
        case psd::colorMode::CMYK:
            return 4;
        case psd::colorMode::MULTICHANNEL:
        default:
            // Every plane is its own ink; there is no alpha to find.
            return channelCount;
    }
}

// The fewest channels a document of this mode can be drawn from: its colour planes. A header that
// declares fewer is malformed (Photoshop cannot write one), and the conversions below would read
// past the planes psd_sdk allocated.
inline unsigned int min_channels(unsigned int colorMode) {
    switch (colorMode) {
        case psd::colorMode::RGB:
        case psd::colorMode::LAB:
            return 3;
        case psd::colorMode::CMYK:
            return 4;
        default:
            return 1;
    }
}

// Alpha is the plane immediately after the colour planes, for EVERY colour mode, not just RGB.
// Decided from the header's channel count, the figure fire_psd_info_get reports, so the caller's
// idea of whether there is an alpha channel is this one. A decode makes one plane per channel.
inline bool header_has_alpha(const psd::Document* document) {
    const unsigned int planes = document->channelCount;
    return planes > color_channel_count(document->colorMode, planes);
}

// Big-endian u16 at `offset` of the document's bytes, or `fallback` past the end.
uint16_t read_be16(const Doc& d, uint64_t offset, uint16_t fallback) {
    if (offset > d.size || d.size - offset < 2) {
        return fallback;
    }
    return static_cast<uint16_t>((d.bytes[offset] << 8) | d.bytes[offset + 1]);
}

// Whether the merged image is stored uncompressed and its planes need more bytes than the file
// has after the compression field. Such a file is truncated, and the patched
// ParseImageDataSection refuses it before allocating a plane (review-psd patch P2); this only
// names the refusal, which psd_sdk can report only as a null section.
bool raw_planes_truncated(const Doc& d) {
    const psd::Section& section = d.document->imageDataSection;
    const uint16_t RAW = 0;
    if (read_be16(d, section.offset, 0xFFFFu) != RAW) {
        return false;
    }
    const uint64_t plane = static_cast<uint64_t>(d.document->width) * d.document->height
        * (d.document->bitsPerChannel / 8u);
    return plane * d.document->channelCount + 2u > section.length;
}

// Read one channel's value at planar index `i`, normalized to 0..1 by source bit depth.
//
// 8- and 16-bit samples are display-encoded (sRGB) and land in 0..1. 32-bit samples are linear
// and deliberately NOT clamped: that is the HDR path, and values above 1.0 are signal, not error.
inline float sample_norm(const void* data, unsigned int bits, size_t i) {
    if (bits == 8) {
        return static_cast<const uint8_t*>(data)[i] * (1.0f / 255.0f);
    }
    if (bits == 16) {
        // Full range, 0..65535. Photoshop *works* in 15-bit+1 (0..32768) internally, but it
        // scales to the full 16-bit range on save. Reading it as 0..32768 instead clipped every
        // sample above mid-scale to white.
        return static_cast<const uint16_t*>(data)[i] * (1.0f / 65535.0f);
    }
    return static_cast<const float*>(data)[i];
}

// sRGB transfer function, for the colour spaces we convert through linear light (Lab).
inline float linear_to_srgb(float c) {
    if (c <= 0.0f) return 0.0f;
    if (c >= 1.0f) return 1.0f;
    return (c <= 0.0031308f) ? (12.92f * c) : (1.055f * std::pow(c, 1.0f / 2.4f) - 0.055f);
}

// CIE L*a*b* (PSD's is D50-referred) to display-encoded sRGB. l/a/b arrive as the raw samples
// normalized to 0..1, i.e. L* = l*100 and a*/b* = (a|b)*255 - 128.
inline void lab_to_srgb(float l, float a, float b, float* out) {
    const float L = l * 100.0f;
    const float A = a * 255.0f - 128.0f;
    const float B = b * 255.0f - 128.0f;

    const float fy = (L + 16.0f) / 116.0f;
    const float fx = fy + A / 500.0f;
    const float fz = fy - B / 200.0f;
    // f⁻¹, with the linear segment below the 6/29 knee.
    const float d = 6.0f / 29.0f;
    const float finv[3] = {
        (fx > d) ? fx * fx * fx : 3.0f * d * d * (fx - 4.0f / 29.0f),
        (fy > d) ? fy * fy * fy : 3.0f * d * d * (fy - 4.0f / 29.0f),
        (fz > d) ? fz * fz * fz : 3.0f * d * d * (fz - 4.0f / 29.0f),
    };
    // D50 reference white (PSD Lab is D50-referred, not D65).
    const float X = 0.96422f * finv[0];
    const float Y = 1.00000f * finv[1];
    const float Z = 0.82521f * finv[2];
    // XYZ(D50) -> linear sRGB, Bradford-adapted.
    out[0] = linear_to_srgb(3.1338561f * X - 1.6168667f * Y - 0.4906146f * Z);
    out[1] = linear_to_srgb(-0.9787684f * X + 1.9161415f * Y + 0.0334540f * Z);
    out[2] = linear_to_srgb(0.0719453f * X - 0.2289914f * Y + 1.4052427f * Z);
}

// Write one normalized sample into the caller's buffer in the document's own depth.
// 8/16-bit clamp (they are display-encoded); 32-bit float passes through, HDR intact.
inline void store_sample(void* out, unsigned int bits, size_t idx, float v) {
    if (bits == 8) {
        const float c = (v < 0.0f) ? 0.0f : (v > 1.0f ? 1.0f : v);
        static_cast<uint8_t*>(out)[idx] = static_cast<uint8_t>(c * 255.0f + 0.5f);
    } else if (bits == 16) {
        const float c = (v < 0.0f) ? 0.0f : (v > 1.0f ? 1.0f : v);
        static_cast<uint16_t*>(out)[idx] = static_cast<uint16_t>(c * 65535.0f + 0.5f);
    } else {
        static_cast<float*>(out)[idx] = v;
    }
}

// Run f(begin, end) over [0, n) on up to `threads` threads, the calling thread taking the first
// part. The count is the caller's (fire-decode's thread budget, FD8); small jobs stay on the
// calling thread. A thread that cannot be started is not an error: the calling thread takes over
// the rest of the range, and every thread that did start is joined before returning (a joinable
// std::thread destroyed unjoined would terminate the process).
template <typename F>
void run_parallel(size_t n, unsigned int threads, F f) {
    const size_t kMinPerThread = size_t(1) << 18;
    if (threads <= 1 || n < 2 * kMinPerThread) {
        f(size_t(0), n);
        return;
    }
    threads = static_cast<unsigned int>(std::min<size_t>(threads, n / kMinPerThread));
    const size_t per = (n + threads - 1) / threads;
    std::vector<std::thread> pool;
    pool.reserve(threads);
    size_t begin = per;
    for (; begin < n; begin += per) {
        try {
            pool.emplace_back(f, begin, std::min(n, begin + per));
        } catch (...) {
            break;
        }
    }
    f(size_t(0), std::min(n, per));
    if (begin < n) {
        f(begin, n);
    }
    for (std::thread& t : pool) {
        t.join();
    }
}

// Interleave up to four planes of `T` samples into RGBA; a null alpha plane is `opaque`.
template <typename T>
void interleave_planes(const void* const src[4], void* out_pixels, size_t pixels, T opaque,
                       unsigned int threads) {
    const T* r = static_cast<const T*>(src[0]);
    const T* g = static_cast<const T*>(src[1]);
    const T* b = static_cast<const T*>(src[2]);
    const T* a = static_cast<const T*>(src[3]);
    T* out = static_cast<T*>(out_pixels);
    run_parallel(pixels, threads, [=](size_t begin, size_t end) {
        T* o = out + begin * 4;
        for (size_t i = begin; i < end; ++i, o += 4) {
            o[0] = r[i];
            o[1] = g[i];
            o[2] = b[i];
            o[3] = a ? a[i] : opaque;
        }
    });
}

// The document against the caller's limits, before anything is sized from it.
int check_document(const psd::Document* document, const fire_psd_limits& limits) {
    const uint64_t w = document->width;
    const uint64_t h = document->height;
    const unsigned int bits = document->bitsPerChannel;
    const unsigned int mode = document->colorMode;
    const unsigned int channels = document->channelCount;
    if (w == 0 || h == 0) {
        return FIRE_PSD_DECODE_FAILED;
    }
    if (!bits_are_supported(bits) || !mode_is_known(mode)
        || channels < min_channels(mode) || channels > limits.max_channels) {
        return FIRE_PSD_UNSUPPORTED;
    }
    if (w > limits.max_dim || h > limits.max_dim) {
        return FIRE_PSD_TOO_LARGE;
    }
    // Two allocations hang off this header and both must fit: psd_sdk's planes (every channel, at
    // the source depth, allocated in C++ below) and the interleaved RGBA output (allocated by the
    // Rust caller). w and h fit in 32 bits each, and channels and bytes per sample are small, so
    // neither product overflows 64 bits.
    const uint64_t bytesPerSample = bits / 8u;
    if (w * h * 4 * bytesPerSample > limits.max_output_bytes
        || w * h * channels * bytesPerSample > limits.max_planar_bytes) {
        return FIRE_PSD_TOO_LARGE;
    }
    return FIRE_PSD_OK;
}

} // namespace

struct fire_psd {
    Doc d;
};

extern "C" {

uint32_t fire_psd_abi_version(void) {
    return FIRE_PSD_ABI_VERSION;
}

fire_psd* fire_psd_open(const uint8_t* bytes, size_t len) {
    if (!bytes || len == 0) {
        return nullptr;
    }
    fire_psd* handle = new (std::nothrow) fire_psd();
    if (!handle) {
        return nullptr;
    }
    // A malformed PSD must never unwind a C++ exception across the FFI boundary into Rust (that
    // is UB). Catch everything and surface it as a null handle.
    try {
        Doc& d = handle->d;
        d.bytes = bytes;
        d.size = len;
        d.file = new (std::nothrow) MemoryFile(&d.allocator, d.bytes, d.size);
        if (!d.file) {
            fire_psd_free(handle);
            return nullptr;
        }
        d.file->OpenRead(L"memory");
        // The header and the section table only: nothing is sized from the header yet, so the
        // caller can read it (fire_psd_info_get) and refuse the document before any pixel data
        // is allocated.
        d.document = psd::CreateDocument(d.file, &d.allocator);
        // Section lengths come from the file and are *skipped*, not read, so one that runs past
        // the end is not a short read: it leaves the image data starting beyond the file, with
        // psd_sdk's `size - offset` length wrapped round to gigabytes.
        if (!d.document || d.file->HadShortRead() || d.document->imageDataSection.offset > d.size) {
            fire_psd_free(handle);
            return nullptr;
        }
        return handle;
    } catch (...) {
        fire_psd_free(handle);
        return nullptr;
    }
}

int fire_psd_info_get(const fire_psd* doc, fire_psd_info* out_info) {
    if (!doc || !out_info || !doc->d.document) {
        return FIRE_PSD_BAD_ARGUMENT;
    }
    const psd::Document* document = doc->d.document;
    out_info->width = document->width;
    out_info->height = document->height;
    out_info->channels = static_cast<uint16_t>(std::min(document->channelCount, 0xFFFFu));
    out_info->bits_per_channel = static_cast<uint16_t>(document->bitsPerChannel);
    out_info->color_mode = static_cast<uint16_t>(document->colorMode);
    out_info->has_alpha = header_has_alpha(document) ? 1 : 0;
    return FIRE_PSD_OK;
}

int fire_psd_decode_merged(fire_psd* doc, const fire_psd_limits* limits) {
    if (!doc || !limits || !doc->d.document) {
        return FIRE_PSD_BAD_ARGUMENT;
    }
    Doc& d = doc->d;
    if (d.imageData) {
        return FIRE_PSD_OK;
    }
    const int checked = check_document(d.document, *limits);
    if (checked != FIRE_PSD_OK) {
        return checked;
    }
    // Merged image is only present when the PSD was saved with Maximize Compatibility.
    if (d.document->imageDataSection.length == 0) {
        return FIRE_PSD_NO_MERGED_IMAGE;
    }
    try {
        d.imageData = psd::ParseImageDataSection(d.document, d.file, &d.allocator);
        // INDEXED documents keep their 256-entry palette here; without it the composite's single
        // plane is a table of indices, and rendering it as grey is noise.
        if (d.document->colorMode == psd::colorMode::INDEXED
            && d.document->colorModeDataSection.length != 0) {
            d.colorModeData = psd::ParseColorModeDataSection(d.document, d.file, &d.allocator);
        }
    } catch (...) {
        return FIRE_PSD_DECODE_FAILED;
    }
    // A truncated document parses "successfully": psd_sdk follows the header's own offsets and
    // never learns it ran off the end, so the composite would be built from the zeros we
    // substituted for the missing bytes. Refuse it rather than display a half-invented image. An
    // uncompressed one is caught before that, by size, and returns no section at all.
    if (d.file->HadShortRead() || (!d.imageData && raw_planes_truncated(d))) {
        return FIRE_PSD_TRUNCATED;
    }
    if (!d.imageData || !d.imageData->images
        || d.imageData->imageCount < d.document->channelCount) {
        return FIRE_PSD_DECODE_FAILED;
    }
    for (unsigned int i = 0; i < d.imageData->imageCount; ++i) {
        if (!d.imageData->images[i].data) {
            return FIRE_PSD_DECODE_FAILED;
        }
    }
    return FIRE_PSD_OK;
}

int fire_psd_read_merged(const fire_psd* doc, void* out_pixels, size_t out_len, uint32_t threads) {
    if (!doc || !out_pixels || !doc->d.document) {
        return FIRE_PSD_BAD_ARGUMENT;
    }
    const Doc& d = doc->d;
    // fire_psd_decode_merged succeeded, which is what vouches for every plane below: one per
    // channel, at least min_channels of them, none null, the depth one of 8/16/32.
    if (!d.imageData || d.file->HadShortRead()) {
        return FIRE_PSD_NOT_DECODED;
    }
    const psd::Document* document = d.document;
    const size_t pixels = static_cast<size_t>(document->width) * document->height;
    const unsigned int bits = document->bitsPerChannel;
    if (out_len / 4 / (bits / 8u) < pixels) {
        return FIRE_PSD_BUFFER_TOO_SMALL;
    }

    const psd::PlanarImage* images = d.imageData->images;
    const unsigned int mode = document->colorMode;
    const unsigned int colorChannels = color_channel_count(mode, document->channelCount);
    const bool hasAlpha = header_has_alpha(document);

    // INDEXED needs its palette: 256 R bytes, then 256 G, then 256 B.
    const uint8_t* palette = nullptr;
    if (mode == psd::colorMode::INDEXED) {
        if (!d.colorModeData || !d.colorModeData->colorData
            || d.colorModeData->sizeOfColorData < 768) {
            return FIRE_PSD_NO_PALETTE;
        }
        palette = d.colorModeData->colorData;
    }

    try {
        // The common documents — 8/16-bit RGB and greyscale — take an integer path. The generic
        // loop below dispatches on the mode and depth for every sample and round-trips each one
        // through float, which for these modes is an exact identity (v/max*max lands back on v),
        // so their planes are interleaved directly. Same bytes, a fraction of the time.
        if ((mode == psd::colorMode::RGB || mode == psd::colorMode::GRAYSCALE
             || mode == psd::colorMode::DUOTONE)
            && (bits == 8 || bits == 16)) {
            const void* src[4];
            for (unsigned int c = 0; c < 3; ++c) {
                src[c] = colorChannels > c ? images[c].data : images[0].data;
            }
            src[3] = hasAlpha ? images[colorChannels].data : nullptr;
            if (bits == 8) {
                interleave_planes<uint8_t>(src, out_pixels, pixels, 255, threads);
            } else {
                interleave_planes<uint16_t>(src, out_pixels, pixels, 65535, threads);
            }
            return FIRE_PSD_OK;
        }

        run_parallel(pixels, threads, [&](size_t begin, size_t end) {
            for (size_t i = begin; i < end; ++i) {
                float rgb[3];
                switch (mode) {
                    case psd::colorMode::INDEXED: {
                        // The single plane holds palette indices, not intensities. Always 8-bit
                        // (Photoshop has no 16-bit indexed mode).
                        const unsigned int idx = static_cast<const uint8_t*>(images[0].data)[i];
                        rgb[0] = palette[idx] * (1.0f / 255.0f);
                        rgb[1] = palette[256 + idx] * (1.0f / 255.0f);
                        rgb[2] = palette[512 + idx] * (1.0f / 255.0f);
                        break;
                    }
                    case psd::colorMode::CMYK: {
                        // PSD stores CMYK *inverted*: 255 means no ink, 0 means full ink. So the
                        // stored values are already (1-ink) and compose straight into RGB:
                        // R = C·K, and so on.
                        const float c = sample_norm(images[0].data, bits, i);
                        const float m = sample_norm(images[1].data, bits, i);
                        const float y = sample_norm(images[2].data, bits, i);
                        const float k = sample_norm(images[3].data, bits, i);
                        rgb[0] = c * k;
                        rgb[1] = m * k;
                        rgb[2] = y * k;
                        break;
                    }
                    case psd::colorMode::LAB: {
                        lab_to_srgb(sample_norm(images[0].data, bits, i),
                                    sample_norm(images[1].data, bits, i),
                                    sample_norm(images[2].data, bits, i), rgb);
                        break;
                    }
                    case psd::colorMode::GRAYSCALE:
                    case psd::colorMode::DUOTONE: {
                        // Duotone is stored as greyscale plus ink metadata we do not model;
                        // showing the grey is the conventional fallback.
                        rgb[0] = rgb[1] = rgb[2] = sample_norm(images[0].data, bits, i);
                        break;
                    }
                    default: {
                        // RGB, and MULTICHANNEL as a best effort (its first three inks).
                        rgb[0] = sample_norm(images[0].data, bits, i);
                        rgb[1] = colorChannels > 1 ? sample_norm(images[1].data, bits, i) : rgb[0];
                        rgb[2] = colorChannels > 2 ? sample_norm(images[2].data, bits, i) : rgb[0];
                        break;
                    }
                }
                const float a = hasAlpha ? sample_norm(images[colorChannels].data, bits, i) : 1.0f;
                store_sample(out_pixels, bits, i * 4 + 0, rgb[0]);
                store_sample(out_pixels, bits, i * 4 + 1, rgb[1]);
                store_sample(out_pixels, bits, i * 4 + 2, rgb[2]);
                store_sample(out_pixels, bits, i * 4 + 3, a);
            }
        });
    } catch (...) {
        return FIRE_PSD_DECODE_FAILED;
    }
    return FIRE_PSD_OK;
}

void fire_psd_free(fire_psd* doc) {
    if (!doc) {
        return;
    }
    Doc& d = doc->d;
    if (d.imageData) {
        psd::DestroyImageDataSection(d.imageData, &d.allocator);
    }
    if (d.colorModeData) {
        psd::DestroyColorModeDataSection(d.colorModeData, &d.allocator);
    }
    if (d.document) {
        psd::DestroyDocument(d.document, &d.allocator);
    }
    if (d.file) {
        d.file->Close();
        delete d.file;
        d.file = nullptr;
    }
    delete doc;
}

} // extern "C"
