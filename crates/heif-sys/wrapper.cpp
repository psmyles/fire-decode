// Implementation of wrapper.h's C ABI (version 2, fire-decode plan FD16) over libheif's C API.
// C++ only so that every entry point can catch every exception: one that reached Rust would
// abort the process (FD7). Compiled through the cc crate against the vendored headers.

#include "wrapper.h"

#include <libheif/heif.h>
#include <libheif/heif_aux_images.h>
#include <libheif/heif_color.h>
#include <libheif/heif_decoding.h>
#include <libheif/heif_items.h>
#include <libheif/heif_security.h>
#include <libheif/heif_sequences.h>
#include <libheif/heif_tiling.h>

#include <algorithm>
#include <atomic>
#include <cstddef>
#include <cstring>
#include <memory>
#include <mutex>
#include <new>
#include <thread>
#include <vector>

// The layout src/bindings.rs asserts on the Rust side (64-bit targets only, which is all build.rs
// accepts). If wrapper.h changes, both sides change together.
static_assert(sizeof(fire_heif_limits) == 40, "fire_heif_limits size");
static_assert(offsetof(fire_heif_limits, max_tiles) == 24, "fire_heif_limits.max_tiles");
static_assert(sizeof(fire_heif_ctl) == 40, "fire_heif_ctl size");
static_assert(offsetof(fire_heif_ctl, ignore_transformations) == 8, "fire_heif_ctl.ignore_transformations");
static_assert(offsetof(fire_heif_ctl, user) == 16, "fire_heif_ctl.user");
static_assert(offsetof(fire_heif_ctl, progress) == 24, "fire_heif_ctl.progress");
static_assert(offsetof(fire_heif_ctl, cancel) == 32, "fire_heif_ctl.cancel");
static_assert(sizeof(fire_heif_info) == 36, "fire_heif_info size");
static_assert(offsetof(fire_heif_info, bit_depth) == 24, "fire_heif_info.bit_depth");
static_assert(offsetof(fire_heif_info, colour_primaries) == 28, "fire_heif_info.colour_primaries");
static_assert(offsetof(fire_heif_info, full_range) == 34, "fire_heif_info.full_range");
static_assert(sizeof(fire_heif_track_info) == 20, "fire_heif_track_info size");
static_assert(sizeof(fire_heif_frame) == 24, "fire_heif_frame size");
static_assert(offsetof(fire_heif_frame, image) == 16, "fire_heif_frame.image");

struct fire_heif {
    heif_context* ctx = nullptr;
    fire_heif_limits limits{};
};

struct fire_heif_track {
    heif_track* track = nullptr;
    fire_heif* file = nullptr;
};

namespace {

// libheif must be initialised before use and must not be deinitialised after exit(), so it can
// go neither in DllMain nor in a static destructor: init once per process, deinit never (FD16).
std::once_flag g_init_once;
heif_error g_init_error;

bool ensure_init() {
    std::call_once(g_init_once, [] { g_init_error = heif_init(nullptr); });
    return g_init_error.code == heif_error_Ok;
}

int status_of(const heif_error& e) {
    if (e.code == heif_error_Ok) return FIRE_HEIF_OK;
    if (e.subcode == heif_suberror_Security_limit_exceeded) return FIRE_HEIF_TOO_LARGE;
    if (e.code == heif_error_Canceled) return FIRE_HEIF_CANCELLED;
    if (e.code == heif_error_End_of_sequence) return FIRE_HEIF_END;
    return static_cast<int>(e.code);
}

struct HandleGuard {
    heif_image_handle* h = nullptr;
    ~HandleGuard() {
        if (h) heif_image_handle_release(h);
    }
};

struct OptionsGuard {
    heif_decoding_options* o = heif_decoding_options_alloc();
    ~OptionsGuard() {
        if (o) heif_decoding_options_free(o);
    }
};

void fill_info(const heif_image_handle* h, uint32_t item_id, fire_heif_info* out) {
    std::memset(out, 0, sizeof(*out));
    out->width = static_cast<uint32_t>(std::max(0, heif_image_handle_get_width(h)));
    out->height = static_cast<uint32_t>(std::max(0, heif_image_handle_get_height(h)));
    out->coded_width = static_cast<uint32_t>(std::max(0, heif_image_handle_get_ispe_width(h)));
    out->coded_height = static_cast<uint32_t>(std::max(0, heif_image_handle_get_ispe_height(h)));
    out->icc_len = static_cast<uint32_t>(heif_image_handle_get_raw_color_profile_size(h));
    out->item_id = item_id;
    const int bits = heif_image_handle_get_luma_bits_per_pixel(h);
    out->bit_depth = static_cast<uint8_t>(std::clamp(bits, 8, 16));
    out->has_alpha = heif_image_handle_has_alpha_channel(h) ? 1 : 0;
    out->premultiplied = heif_image_handle_is_premultiplied_alpha(h) ? 1 : 0;
    out->colour_primaries = 2;
    out->transfer_characteristics = 2;
    out->matrix_coefficients = 2;
    heif_color_profile_nclx* nclx = nullptr;
    if (heif_image_handle_get_nclx_color_profile(h, &nclx).code == heif_error_Ok && nclx) {
        out->has_nclx = 1;
        out->colour_primaries = static_cast<uint16_t>(nclx->color_primaries);
        out->transfer_characteristics = static_cast<uint16_t>(nclx->transfer_characteristics);
        out->matrix_coefficients = static_cast<uint16_t>(nclx->matrix_coefficients);
        out->full_range = nclx->full_range_flag ? 1 : 0;
    }
    if (nclx) heif_nclx_color_profile_free(nclx);
}

bool wants_16bit(unsigned bits, const fire_heif_ctl* ctl) {
    return bits > 8 || (ctl && ctl->force_16bit);
}

// The early refusal (FD16): the signalled size against the caller's limits, before libheif
// decodes anything. libheif notes the decoded size may differ, so frame_read checks again.
int check_size(const fire_heif* file, uint64_t w, uint64_t h, bool use16) {
    const fire_heif_limits& l = file->limits;
    if (w == 0 || h == 0) return FIRE_HEIF_BAD_IMAGE;
    if (l.max_dim && (w > l.max_dim || h > l.max_dim)) return FIRE_HEIF_TOO_LARGE;
    if (l.max_output_bytes && w * h * (use16 ? 8u : 4u) > l.max_output_bytes) return FIRE_HEIF_TOO_LARGE;
    return FIRE_HEIF_OK;
}

// libheif's progress and cancel callbacks, forwarded to the caller's. They may run on libheif's
// own threads; the caller's are thread-safe by contract (wrapper.h).
struct Progress {
    const fire_heif_ctl* ctl;
    std::atomic<int> max{0};
    std::atomic<bool> cancelled{false};
};

void on_start(heif_progress_step, int max_progress, void* user) {
    auto* p = static_cast<Progress*>(user);
    p->max.store(max_progress);
    if (p->ctl->progress) p->ctl->progress(p->ctl->user, 0, static_cast<uint32_t>(std::max(0, max_progress)));
}

void on_progress(heif_progress_step, int progress, void* user) {
    auto* p = static_cast<Progress*>(user);
    if (p->ctl->progress) {
        p->ctl->progress(p->ctl->user, static_cast<uint32_t>(std::max(0, progress)),
                         static_cast<uint32_t>(std::max(0, p->max.load())));
    }
}

void on_end(heif_progress_step, void* user) {
    auto* p = static_cast<Progress*>(user);
    const uint32_t max = static_cast<uint32_t>(std::max(0, p->max.load()));
    if (p->ctl->progress) p->ctl->progress(p->ctl->user, max, max);
}

int on_cancel(void* user) {
    auto* p = static_cast<Progress*>(user);
    if (p->ctl->cancel && p->ctl->cancel(p->ctl->user)) {
        p->cancelled.store(true);
        return 1;
    }
    return 0;
}

// The decoding options for one decode, from the caller's controls.
void configure(heif_decoding_options* o, const fire_heif_ctl* ctl, Progress* progress) {
    o->ignore_sequence_editlist = 1;  // FD18: play the media timeline once; looping is the caller's
    if (!ctl) return;
    o->ignore_transformations = ctl->ignore_transformations ? 1 : 0;
    o->num_codec_threads = static_cast<int>(ctl->codec_threads);
    if (ctl->keep_source_color) o->output_image_nclx_profile_passthrough = 1;
    if (ctl->progress || ctl->cancel) {
        o->progress_user_data = progress;
        o->start_progress = on_start;
        o->on_progress = on_progress;
        o->end_progress = on_end;
        o->cancel_decoding = on_cancel;
    }
}

int to_frame(heif_image* img, unsigned luma_bits, bool has_alpha, bool premultiplied, bool use16,
             fire_heif_frame* out) {
    const int w = heif_image_get_width(img, heif_channel_interleaved);
    const int h = heif_image_get_height(img, heif_channel_interleaved);
    size_t stride = 0;
    const uint8_t* plane = heif_image_get_plane_readonly2(img, heif_channel_interleaved, &stride);
    const size_t row = static_cast<size_t>(std::max(0, w)) * (use16 ? 8u : 4u);
    if (w <= 0 || h <= 0 || !plane || stride < row || (use16 && stride % 2 != 0)) {
        heif_image_release(img);
        return FIRE_HEIF_BAD_IMAGE;
    }
    out->width = static_cast<uint32_t>(w);
    out->height = static_cast<uint32_t>(h);
    out->duration = heif_image_get_duration(img);
    // The range the samples use, which bit replication scales to 16 bits.
    const int range = heif_image_get_bits_per_pixel_range(img, heif_channel_interleaved);
    out->bit_depth = static_cast<uint8_t>(std::clamp(use16 ? range : static_cast<int>(luma_bits), 8, 16));
    out->has_alpha = has_alpha ? 1 : 0;
    // Premultiplication is a flag on the handle or the image, not part of the chroma: asking for
    // interleaved RGBA does not straighten anything.
    out->premultiplied = (premultiplied || heif_image_is_premultiplied_alpha(img)) ? 1 : 0;
    out->is_16bit = use16 ? 1 : 0;
    out->image = img;
    return FIRE_HEIF_OK;
}

int decode_handle(fire_heif* file, const heif_image_handle* h, const fire_heif_ctl* ctl,
                  fire_heif_frame* out) {
    std::memset(out, 0, sizeof(*out));
    const unsigned bits = static_cast<unsigned>(std::clamp(heif_image_handle_get_luma_bits_per_pixel(h), 8, 16));
    const bool use16 = wants_16bit(bits, ctl);
    const int sized = check_size(file, static_cast<uint64_t>(std::max(0, heif_image_handle_get_width(h))),
                                 static_cast<uint64_t>(std::max(0, heif_image_handle_get_height(h))), use16);
    if (sized != FIRE_HEIF_OK) return sized;
    OptionsGuard options;
    if (!options.o) return static_cast<int>(heif_error_Memory_allocation_error);
    Progress progress{ctl};
    configure(options.o, ctl, &progress);
    if (ctl && ctl->library_threads) {
        uint32_t library = ctl->library_threads;
        uint32_t codec = ctl->codec_threads;
        if (codec == 0) {
            // FD8: tiles in parallel for a grid, the codec's own threads for a single image.
            heif_image_tiling tiling{};
            const bool grid = heif_image_handle_get_image_tiling(h, 0, &tiling).code == heif_error_Ok
                              && uint64_t(tiling.num_columns) * tiling.num_rows > 1;
            codec = grid ? 1 : library;
            library = grid ? library : 1;
        }
        heif_context_set_max_decoding_threads(file->ctx, static_cast<int>(library));
        options.o->num_codec_threads = static_cast<int>(codec);
    }
    heif_image* img = nullptr;
    const heif_error e = heif_decode_image(
        h, &img, heif_colorspace_RGB,
        use16 ? heif_chroma_interleaved_RRGGBBAA_LE : heif_chroma_interleaved_RGBA, options.o);
    if (e.code != heif_error_Ok || !img) {
        if (img) heif_image_release(img);
        if (progress.cancelled.load()) return FIRE_HEIF_CANCELLED;
        return e.code == heif_error_Ok ? FIRE_HEIF_BAD_IMAGE : status_of(e);
    }
    return to_frame(img, bits, heif_image_handle_has_alpha_channel(h) != 0,
                    heif_image_handle_is_premultiplied_alpha(h) != 0, use16, out);
}

int aux_handle(fire_heif* file, uint32_t item_id, uint32_t aux_id, HandleGuard& aux) {
    HandleGuard main;
    const heif_error e = heif_context_get_image_handle(file->ctx, item_id, &main.h);
    if (e.code != heif_error_Ok) return FIRE_HEIF_NOT_FOUND;
    if (heif_image_handle_get_auxiliary_image_handle(main.h, aux_id, &aux.h).code != heif_error_Ok) {
        return FIRE_HEIF_NOT_FOUND;
    }
    return FIRE_HEIF_OK;
}

// Run f(begin, end) over rows [0, n) on up to `threads` threads, the calling thread included.
// A thread that cannot be started is not an error: the calling thread does its share.
template <typename F>
void run_parallel(size_t n, size_t pixels, unsigned threads, F f) {
    const size_t kMinPixelsPerThread = size_t(1) << 18;
    if (threads <= 1 || n < 2 || pixels < 2 * kMinPixelsPerThread) {
        f(size_t(0), n);
        return;
    }
    threads = static_cast<unsigned>(std::min<size_t>({threads, n, pixels / kMinPixelsPerThread}));
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
    if (begin < n) f(begin, n);
    for (std::thread& t : pool) t.join();
}

} // namespace

extern "C" {

uint32_t fire_heif_abi_version(void) {
    return FIRE_HEIF_ABI_VERSION;
}

fire_heif* fire_heif_open(const uint8_t* data, size_t len, const fire_heif_limits* limits,
                          int* out_status) {
    int ignored = 0;
    int& status = out_status ? *out_status : ignored;
    status = FIRE_HEIF_OK;
    if (!data || len == 0 || !limits) {
        status = FIRE_HEIF_BAD_ARGUMENT;
        return nullptr;
    }
    try {
        if (!ensure_init()) {
            status = FIRE_HEIF_INIT_FAILED;
            return nullptr;
        }
        std::unique_ptr<fire_heif> file(new (std::nothrow) fire_heif());
        if (!file) {
            status = static_cast<int>(heif_error_Memory_allocation_error);
            return nullptr;
        }
        file->limits = *limits;
        file->ctx = heif_context_alloc();
        if (!file->ctx) {
            status = static_cast<int>(heif_error_Memory_allocation_error);
            return nullptr;
        }
        // libheif's own limits, from the caller's (FD16): the image area an RGBA8 output within
        // the byte limit could hold, total memory, tiles, sequence frames, colour-profile size.
        // The rest keep libheif's defaults.
        if (heif_security_limits* sl = heif_context_get_security_limits(file->ctx)) {
            if (limits->max_output_bytes) {
                uint64_t pixels = limits->max_output_bytes / 4;
                if (limits->max_dim) pixels = std::min<uint64_t>(pixels, limits->max_dim * limits->max_dim);
                if (sl->max_image_size_pixels == 0 || pixels < sl->max_image_size_pixels) {
                    sl->max_image_size_pixels = pixels;
                }
            }
            if (limits->max_total_memory) sl->max_total_memory = limits->max_total_memory;
            if (limits->max_tiles) sl->max_number_of_tiles = limits->max_tiles;
            if (limits->max_sequence_frames) sl->max_sequence_frames = limits->max_sequence_frames;
            if (limits->max_color_profile_size) sl->max_color_profile_size = limits->max_color_profile_size;
        }
        const heif_error e = heif_context_read_from_memory_without_copy(file->ctx, data, len, nullptr);
        if (e.code != heif_error_Ok) {
            status = status_of(e);
            heif_context_free(file->ctx);
            return nullptr;
        }
        return file.release();
    } catch (...) {
        status = FIRE_HEIF_EXCEPTION;
        return nullptr;
    }
}

void fire_heif_free(fire_heif* file) {
    if (!file) return;
    try {
        if (file->ctx) heif_context_free(file->ctx);
    } catch (...) {
    }
    delete file;
}

int fire_heif_primary_id(const fire_heif* file, uint32_t* out_id) {
    if (!file || !out_id) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        heif_item_id id = 0;
        if (heif_context_get_primary_image_ID(file->ctx, &id).code != heif_error_Ok) return FIRE_HEIF_NO_IMAGE;
        *out_id = id;
        return FIRE_HEIF_OK;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_item_info(fire_heif* file, uint32_t item_id, fire_heif_info* out_info) {
    if (!file || !out_info) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        HandleGuard h;
        const heif_error e = heif_context_get_image_handle(file->ctx, item_id, &h.h);
        if (e.code != heif_error_Ok) return e.subcode == heif_suberror_Security_limit_exceeded ? FIRE_HEIF_TOO_LARGE : FIRE_HEIF_NOT_FOUND;
        fill_info(h.h, item_id, out_info);
        return FIRE_HEIF_OK;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_item_icc(fire_heif* file, uint32_t item_id, uint8_t* out, size_t len) {
    if (!file || !out) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        HandleGuard h;
        if (heif_context_get_image_handle(file->ctx, item_id, &h.h).code != heif_error_Ok) return FIRE_HEIF_NOT_FOUND;
        const size_t size = heif_image_handle_get_raw_color_profile_size(h.h);
        if (size == 0) return FIRE_HEIF_NOT_FOUND;
        if (len < size) return FIRE_HEIF_BUFFER_TOO_SMALL;
        return status_of(heif_image_handle_get_raw_color_profile(h.h, out));
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_decode_item(fire_heif* file, uint32_t item_id, const fire_heif_ctl* ctl,
                          fire_heif_frame* out_frame) {
    if (!file || !out_frame) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        std::memset(out_frame, 0, sizeof(*out_frame));
        HandleGuard h;
        const heif_error e = heif_context_get_image_handle(file->ctx, item_id, &h.h);
        if (e.code != heif_error_Ok) return status_of(e);
        return decode_handle(file, h.h, ctl, out_frame);
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_aux_find(fire_heif* file, uint32_t item_id, const char* urn, uint32_t* out_aux_id) {
    if (!file || !urn || !out_aux_id) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        HandleGuard main;
        if (heif_context_get_image_handle(file->ctx, item_id, &main.h).code != heif_error_Ok) return FIRE_HEIF_NOT_FOUND;
        const int n = heif_image_handle_get_number_of_auxiliary_images(main.h, 0);
        if (n <= 0) return FIRE_HEIF_NOT_FOUND;
        std::vector<heif_item_id> ids(static_cast<size_t>(n));
        const int got = heif_image_handle_get_list_of_auxiliary_image_IDs(main.h, 0, ids.data(), n);
        for (int i = 0; i < got; ++i) {
            HandleGuard aux;
            if (heif_image_handle_get_auxiliary_image_handle(main.h, ids[i], &aux.h).code != heif_error_Ok) continue;
            const char* type = nullptr;
            if (heif_image_handle_get_auxiliary_type(aux.h, &type).code != heif_error_Ok) continue;
            const bool match = type && std::strcmp(type, urn) == 0;
            heif_image_handle_release_auxiliary_type(aux.h, &type);
            if (match) {
                *out_aux_id = ids[i];
                return FIRE_HEIF_OK;
            }
        }
        return FIRE_HEIF_NOT_FOUND;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_aux_info(fire_heif* file, uint32_t item_id, uint32_t aux_id, fire_heif_info* out_info) {
    if (!file || !out_info) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        HandleGuard aux;
        const int found = aux_handle(file, item_id, aux_id, aux);
        if (found != FIRE_HEIF_OK) return found;
        fill_info(aux.h, aux_id, out_info);
        return FIRE_HEIF_OK;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_decode_aux(fire_heif* file, uint32_t item_id, uint32_t aux_id,
                         const fire_heif_ctl* ctl, fire_heif_frame* out_frame) {
    if (!file || !out_frame) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        std::memset(out_frame, 0, sizeof(*out_frame));
        HandleGuard aux;
        const int found = aux_handle(file, item_id, aux_id, aux);
        if (found != FIRE_HEIF_OK) return found;
        return decode_handle(file, aux.h, ctl, out_frame);
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_item_data(fire_heif* file, uint32_t item_id, uint8_t* out, size_t len, size_t* out_len) {
    if (!file || !out_len) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        uint8_t* data = nullptr;
        size_t size = 0;
        const heif_error e = heif_item_get_item_data(file->ctx, item_id, nullptr, &data, &size);
        if (e.code != heif_error_Ok) {
            if (data) heif_release_item_data(file->ctx, &data);
            return status_of(e);
        }
        *out_len = size;
        int status = FIRE_HEIF_OK;
        if (out) {
            if (len < size) {
                status = FIRE_HEIF_BUFFER_TOO_SMALL;
            } else if (size) {
                std::memcpy(out, data, size);
            }
        }
        if (data) heif_release_item_data(file->ctx, &data);
        return status;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

fire_heif_track* fire_heif_track_open(fire_heif* file, int* out_status) {
    int ignored = 0;
    int& status = out_status ? *out_status : ignored;
    status = FIRE_HEIF_OK;
    if (!file) {
        status = FIRE_HEIF_BAD_ARGUMENT;
        return nullptr;
    }
    try {
        const int n = heif_context_number_of_sequence_tracks(file->ctx);
        if (n <= 0) {
            status = FIRE_HEIF_NO_IMAGE;
            return nullptr;
        }
        std::vector<uint32_t> ids(static_cast<size_t>(n));
        heif_context_get_track_ids(file->ctx, ids.data());
        for (uint32_t id : ids) {
            heif_track* track = heif_context_get_track(file->ctx, id);
            if (!track) continue;
            const heif_track_type type = heif_track_get_track_handler_type(track);
            if (type == heif_track_type_video || type == heif_track_type_image_sequence) {
                fire_heif_track* out = new (std::nothrow) fire_heif_track();
                if (!out) {
                    heif_track_release(track);
                    status = static_cast<int>(heif_error_Memory_allocation_error);
                    return nullptr;
                }
                out->track = track;
                out->file = file;
                return out;
            }
            heif_track_release(track);
        }
        status = FIRE_HEIF_NO_IMAGE;
        return nullptr;
    } catch (...) {
        status = FIRE_HEIF_EXCEPTION;
        return nullptr;
    }
}

int fire_heif_track_info_get(const fire_heif_track* track, fire_heif_track_info* out_info) {
    if (!track || !out_info) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        std::memset(out_info, 0, sizeof(*out_info));
        uint16_t w = 0, h = 0;
        const heif_error e = heif_track_get_image_resolution(track->track, &w, &h);
        if (e.code != heif_error_Ok) return status_of(e);
        out_info->width = w;
        out_info->height = h;
        out_info->timescale = heif_track_get_timescale(track->track);
        out_info->repetitions = heif_track_get_number_of_repetitions(track->track);
        out_info->has_alpha = heif_track_has_alpha_channel(track->track) ? 1 : 0;
        return FIRE_HEIF_OK;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

int fire_heif_track_next(fire_heif_track* track, const fire_heif_ctl* ctl, fire_heif_frame* out_frame) {
    if (!track || !out_frame) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        std::memset(out_frame, 0, sizeof(*out_frame));
        uint16_t w = 0, h = 0;
        heif_error e = heif_track_get_image_resolution(track->track, &w, &h);
        if (e.code != heif_error_Ok) return status_of(e);
        // A track has no handle to read a depth from before decoding: it decodes to RGBA8 unless
        // the caller asks for 16 bits.
        const bool use16 = ctl && ctl->force_16bit;
        const int sized = check_size(track->file, w, h, use16);
        if (sized != FIRE_HEIF_OK) return sized;
        OptionsGuard options;
        if (!options.o) return static_cast<int>(heif_error_Memory_allocation_error);
        Progress progress{ctl};
        configure(options.o, ctl, &progress);
        if (ctl && ctl->library_threads) {
            // A sequence frame is a single coded image: the budget goes to the codec (FD8).
            heif_context_set_max_decoding_threads(track->file->ctx, 1);
            options.o->num_codec_threads = static_cast<int>(ctl->codec_threads ? ctl->codec_threads : ctl->library_threads);
        }
        heif_image* img = nullptr;
        e = heif_track_decode_next_image(
            track->track, &img, heif_colorspace_RGB,
            use16 ? heif_chroma_interleaved_RRGGBBAA_LE : heif_chroma_interleaved_RGBA, options.o);
        if (e.code != heif_error_Ok || !img) {
            if (img) heif_image_release(img);
            if (progress.cancelled.load()) return FIRE_HEIF_CANCELLED;
            return e.code == heif_error_Ok ? FIRE_HEIF_BAD_IMAGE : status_of(e);
        }
        return to_frame(img, 8, heif_track_has_alpha_channel(track->track) != 0, false, use16, out_frame);
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

void fire_heif_track_free(fire_heif_track* track) {
    if (!track) return;
    try {
        if (track->track) heif_track_release(track->track);
    } catch (...) {
    }
    delete track;
}

int fire_heif_frame_read(const fire_heif_frame* frame, void* out, size_t len, uint32_t threads) {
    if (!frame || !frame->image || !out) return FIRE_HEIF_BAD_ARGUMENT;
    try {
        const heif_image* img = static_cast<const heif_image*>(frame->image);
        size_t stride = 0;
        const uint8_t* plane = heif_image_get_plane_readonly2(img, heif_channel_interleaved, &stride);
        const bool use16 = frame->is_16bit != 0;
        const size_t w = frame->width, h = frame->height;
        const size_t row = w * (use16 ? 8u : 4u);
        if (!plane || stride < row) return FIRE_HEIF_BAD_IMAGE;
        if (len / row < h) return FIRE_HEIF_BUFFER_TOO_SMALL;
        const bool straighten = frame->has_alpha && frame->premultiplied;
        const unsigned bits = std::clamp<unsigned>(frame->bit_depth, 8, 16);
        uint8_t* dst = static_cast<uint8_t*>(out);
        run_parallel(h, w * h, threads, [&](size_t begin, size_t end) {
            for (size_t y = begin; y < end; ++y) {
                const uint8_t* src_row = plane + y * stride;
                uint8_t* dst_row = dst + y * row;
                if (!use16) {
                    std::memcpy(dst_row, src_row, row);
                    if (!straighten) continue;
                    for (size_t i = 0; i < row; i += 4) {
                        const unsigned a = dst_row[i + 3];
                        if (a == 0) {
                            dst_row[i] = dst_row[i + 1] = dst_row[i + 2] = 0;
                        } else if (a < 255) {
                            for (int c = 0; c < 3; ++c) {
                                const unsigned v = (dst_row[i + c] * 255u + a / 2u) / a;
                                dst_row[i + c] = static_cast<uint8_t>(v > 255u ? 255u : v);
                            }
                        }
                    }
                } else {
                    // Right-aligned `bits`-bit samples scaled to 0..65535 by bit replication, so
                    // the maximum maps to 65535 exactly: 10-bit is (v << 6) | (v >> 4).
                    const unsigned shift = 16 - bits;
                    const auto* s = reinterpret_cast<const uint16_t*>(src_row);
                    auto* d = reinterpret_cast<uint16_t*>(dst_row);
                    for (size_t i = 0; i < w * 4; ++i) {
                        const uint32_t v = s[i];
                        d[i] = static_cast<uint16_t>(shift ? (v << shift) | (v >> (bits - shift)) : v);
                    }
                    if (!straighten) continue;
                    for (size_t i = 0; i < w * 4; i += 4) {
                        const uint32_t a = d[i + 3];
                        if (a == 0) {
                            d[i] = d[i + 1] = d[i + 2] = 0;
                        } else if (a < 65535u) {
                            for (int c = 0; c < 3; ++c) {
                                const uint32_t v = (d[i + c] * 65535u + a / 2u) / a;
                                d[i + c] = static_cast<uint16_t>(v > 65535u ? 65535u : v);
                            }
                        }
                    }
                }
            }
        });
        return FIRE_HEIF_OK;
    } catch (...) {
        return FIRE_HEIF_EXCEPTION;
    }
}

void fire_heif_frame_free(fire_heif_frame* frame) {
    if (!frame) return;
    try {
        if (frame->image) heif_image_release(static_cast<heif_image*>(frame->image));
    } catch (...) {
    }
    std::memset(frame, 0, sizeof(*frame));
}

} // extern "C"
