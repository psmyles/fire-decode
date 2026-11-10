/*
 * C-ABI wrapper over libheif (HEIC/HEIF via libde265, AVIF via dav1d).
 *
 * Like psd-sdk-sys/wrapper.h, this header is deliberately C-style, <stdint.h> / <stddef.h>
 * only. Its Rust side is hand-kept in src/bindings.rs, and a plain C ABI keeps that file small
 * and its layout checkable from both sides. The implementation (wrapper.c) includes
 * <libheif/heif.h> and is compiled by the platform's C compiler through the cc crate.
 *
 * The wrapper collapses libheif's multi-call decode (context -> handle -> image -> plane)
 * into a single `fire_heif_decode` that hands Rust an interleaved-RGBA buffer plus the
 * source bit depth and any embedded ICC profile. Buffers are malloc'd C-side and released
 * by `fire_heif_image_free` so allocation and free stay on one heap (the one CRT the crate links).
 */
#ifndef FIRE_HEIF_WRAPPER_H
#define FIRE_HEIF_WRAPPER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A decoded HEIF/AVIF primary image, normalized to interleaved RGBA. */
typedef struct fire_heif_image {
    uint32_t width;
    uint32_t height;
    /* Source luma bits per channel: 8, 10, or 12. */
    uint8_t bit_depth;
    /* 1 if the source carried an alpha channel, else 0 (pixels are RGBA regardless). */
    uint8_t has_alpha;
    /* Pixel layout: 0 => 8-bit RGBA (4 bytes/px); 1 => 16-bit RGBA, native-endian u16,
     * scaled to full 0..65535 range (8 bytes/px). Set when bit_depth > 8. */
    uint8_t is_16bit;
    uint8_t _pad;
    /* Interleaved RGBA, row-major, malloc'd. NULL on failure. */
    uint8_t* pixels;
    size_t pixels_len;
    /* Embedded ICC profile bytes (malloc'd) or NULL if none. */
    uint8_t* icc;
    size_t icc_len;
} fire_heif_image;

/* Decode the primary image of a HEIC/HEIF/AVIF from an in-memory buffer.
 * Returns 0 on success (fills *out); non-zero libheif/internal error code otherwise.
 * On failure *out is zeroed (no buffers to free). */
int fire_heif_decode(const uint8_t* data, size_t len, fire_heif_image* out);

/* Free the malloc'd buffers (pixels, icc) inside *img and zero the struct.
 * Safe to call on a zeroed/failed image (no-op). */
void fire_heif_image_free(fire_heif_image* img);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* FIRE_HEIF_WRAPPER_H */
