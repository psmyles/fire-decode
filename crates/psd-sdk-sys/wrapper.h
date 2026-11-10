/*
 * C-ABI wrapper over the vendored psd_sdk C++ library (Molecular Matters): ABI version 2, the
 * merge of fire's and 3D Review's wrappers (fire-decode plan, FD3).
 *
 * This header is deliberately C-style: <stdint.h> / <stddef.h> only, never a C++
 * standard-library header. Its Rust side is hand-kept in src/bindings.rs, and a plain C ABI
 * keeps that file small and its layout checkable from both sides. The psd_sdk C++ behind it
 * (which does use the STL) is compiled by the platform's C++ compiler through the cc crate.
 *
 * The decode is staged so nothing is sized from the header before the caller has seen it:
 *   1. fire_psd_open reads the header and the section table only;
 *   2. fire_psd_info_get reports them;
 *   3. fire_psd_decode_merged checks the document against the caller's limits, then has psd_sdk
 *      allocate and decode the merged image's planes;
 *   4. fire_psd_read_merged interleaves them into the caller's buffer, at the document's depth.
 * Every entry point catches every C++ exception; none crosses into Rust.
 *
 * The implementation lives in wrapper.cpp.
 */

#ifndef FIRE_PSD_WRAPPER_H
#define FIRE_PSD_WRAPPER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FIRE_PSD_ABI_VERSION 2u

/* Status codes. */
#define FIRE_PSD_OK 0
#define FIRE_PSD_BAD_ARGUMENT 1      /* a null pointer, or a call out of order */
#define FIRE_PSD_UNSUPPORTED 2       /* a depth, colour mode or channel count with no defined drawing */
#define FIRE_PSD_TOO_LARGE 3         /* past the caller's limits; nothing was allocated */
#define FIRE_PSD_NO_MERGED_IMAGE 4   /* saved without "Maximize Compatibility" */
#define FIRE_PSD_TRUNCATED 5         /* the file ends before the data its header describes */
#define FIRE_PSD_DECODE_FAILED 6     /* malformed image data, e.g. a corrupt RLE stream */
#define FIRE_PSD_NO_PALETTE 7        /* an indexed document without its 256-colour table */
#define FIRE_PSD_NOT_DECODED 8       /* fire_psd_read_merged before a successful decode */
#define FIRE_PSD_BUFFER_TOO_SMALL 9

/* An opened document. The bytes passed to fire_psd_open are borrowed, unmoved and unmodified,
 * until fire_psd_free: the decode reads them in place rather than copying the file. */
typedef struct fire_psd fire_psd;

/* The header. */
typedef struct fire_psd_info {
    uint32_t width;
    uint32_t height;
    /* Every plane the document stores, spot channels and saved selections included. */
    uint16_t channels;
    /* 1, 8, 16 or 32 (only 8, 16 and 32 decode). */
    uint16_t bits_per_channel;
    /* psd_sdk's colorMode::Enum: 0 bitmap, 1 greyscale, 2 indexed, 3 RGB, 4 CMYK,
     * 7 multichannel, 8 duotone, 9 Lab. */
    uint16_t color_mode;
    /* 1 if the plane after the colour planes is the composite's transparency, else 0. */
    uint16_t has_alpha;
} fire_psd_info;

/* What the caller will allow a decode to allocate (FD5's per-setup limits). */
typedef struct fire_psd_limits {
    /* Per axis. */
    uint64_t max_dim;
    /* The interleaved RGBA the caller allocates for fire_psd_read_merged, at the document's depth. */
    uint64_t max_output_bytes;
    /* The planes psd_sdk holds while decoding: every channel, at the document's depth. */
    uint64_t max_planar_bytes;
    uint32_t max_channels;
    uint32_t reserved;
} fire_psd_limits;

uint32_t fire_psd_abi_version(void);

/* Open a document from bytes in memory: the header and the section table only. NULL if they
 * cannot be read, or a section runs past the end of the file. */
fire_psd* fire_psd_open(const uint8_t* bytes, size_t len);

int fire_psd_info_get(const fire_psd* doc, fire_psd_info* out_info);

/* Check the document against `limits`, then decode the merged image's planes. Idempotent once it
 * has succeeded. */
int fire_psd_decode_merged(fire_psd* doc, const fire_psd_limits* limits);

/* Interleave the decoded planes into `out_pixels` as RGBA at the document's depth: u8, native-
 * endian u16, or native-endian f32 (linear, unclamped) for 8-, 16- and 32-bit documents. Colour
 * modes other than RGB and greyscale are converted to sRGB. Uses up to `threads` threads, the
 * calling thread included; 0 or 1 means the calling thread only. */
int fire_psd_read_merged(const fire_psd* doc, void* out_pixels, size_t out_len, uint32_t threads);

void fire_psd_free(fire_psd* doc);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* FIRE_PSD_WRAPPER_H */
