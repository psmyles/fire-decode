/*
 * C-ABI wrapper over libheif (HEIC / HEIF via libde265, AVIF via dav1d): ABI version 2
 * (fire-decode plan, FD16).
 *
 * Like psd-sdk-sys/wrapper.h, this header is deliberately C-style, <stdint.h> / <stddef.h> only.
 * Its Rust side is hand-kept in src/bindings.rs, and a plain C ABI keeps that file small and its
 * layout checkable from both sides. The implementation (wrapper.cpp) is C++, so that every entry
 * point can catch every exception: none crosses into Rust (FD7).
 *
 * The ABI covers what migration steps 2b-6 need, so the bindings change once:
 *   - a file:      fire_heif_open, with libheif's security limits set from the caller's (FD16);
 *   - its images:  the primary item, any item by ID, an item's auxiliary images by type, and raw
 *                  item data (gain-map metadata, FD17);
 *   - sequences:   the first visual track, decoded frame by frame (FD18);
 *   - a decode:    thread counts, progress and cancel hooks, ignore_transformations, and whether
 *                  to keep the source's colour (nclx passthrough, FD17), all in fire_heif_ctl;
 *   - its pixels:  a fire_heif_frame holds libheif's decoded image, and fire_heif_frame_read
 *                  copies it once, straight into the caller's buffer.
 *
 * Status codes: FIRE_HEIF_OK, a negative FIRE_HEIF_* below, or a positive libheif
 * heif_error_code.
 */

#ifndef FIRE_HEIF_WRAPPER_H
#define FIRE_HEIF_WRAPPER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FIRE_HEIF_ABI_VERSION 2u

#define FIRE_HEIF_OK 0
#define FIRE_HEIF_BAD_ARGUMENT -1
#define FIRE_HEIF_TOO_LARGE -2        /* past the caller's limits, refused before decoding */
#define FIRE_HEIF_CANCELLED -3
#define FIRE_HEIF_NO_IMAGE -4         /* neither a primary image nor a visual track */
#define FIRE_HEIF_BAD_IMAGE -5        /* libheif returned an image without a usable plane */
#define FIRE_HEIF_EXCEPTION -6        /* a C++ exception, caught at the boundary */
#define FIRE_HEIF_BUFFER_TOO_SMALL -7
#define FIRE_HEIF_NOT_FOUND -8        /* no such item, or no auxiliary image of that type */
#define FIRE_HEIF_END -9              /* a sequence has no more frames */
#define FIRE_HEIF_INIT_FAILED -10     /* heif_init failed: no decoders */

/* What a file may make libheif allocate (FD5's per-setup limits). 0 leaves libheif's default. */
typedef struct fire_heif_limits {
    uint64_t max_dim;             /* per axis, checked before any decode */
    uint64_t max_output_bytes;    /* the RGBA the caller allocates; also bounds libheif's pixels */
    uint64_t max_total_memory;    /* libheif's own budget for one file */
    uint32_t max_tiles;
    uint32_t max_sequence_frames;
    uint32_t max_color_profile_size;
    uint32_t reserved;
} fire_heif_limits;

/* How to run one decode. The callbacks may be called from libheif's own threads. */
typedef struct fire_heif_ctl {
    /* The thread budget (FD8). With codec_threads 0, the wrapper splits it by the item's tiling:
     * a grid gets library_threads tile threads with one codec thread each, a single image one
     * tile thread with library_threads codec threads. Otherwise both are taken as given.
     * 0 for both leaves libheif's and the codecs' defaults. */
    uint32_t library_threads;
    uint32_t codec_threads;
    uint8_t ignore_transformations; /* 1: leave irot / imir / clap unapplied (gain maps, FD17) */
    uint8_t keep_source_color;    /* 1: output in the source's nclx (HDR, FD17); 0: sRGB */
    uint8_t force_16bit;          /* 1: RGBA16 even from an 8-bit source; 0: by source depth */
    uint8_t reserved;
    uint32_t reserved2;
    void* user;
    void (*progress)(void* user, uint32_t done, uint32_t total); /* may be NULL */
    int (*cancel)(void* user);    /* may be NULL; non-zero stops the decode */
} fire_heif_ctl;

/* An image item, before decoding. */
typedef struct fire_heif_info {
    uint32_t width;               /* as shown, after irot / imir / clap (signalled, not decoded) */
    uint32_t height;
    uint32_t coded_width;         /* ispe: as stored */
    uint32_t coded_height;
    uint32_t icc_len;             /* 0 if none */
    uint32_t item_id;
    uint8_t bit_depth;            /* luma bits: 8, 10, 12 */
    uint8_t has_alpha;
    uint8_t premultiplied;
    uint8_t has_nclx;
    uint16_t colour_primaries;    /* nclx, ITU-T H.273; 2 (unspecified) if none */
    uint16_t transfer_characteristics;
    uint16_t matrix_coefficients;
    uint8_t full_range;
    uint8_t reserved;
} fire_heif_info;

/* The first visual track of an image sequence. */
typedef struct fire_heif_track_info {
    uint32_t width;
    uint32_t height;
    uint32_t timescale;           /* ticks a second */
    uint32_t repetitions;         /* 0xFFFFFFFF = infinite */
    uint8_t has_alpha;
    uint8_t reserved[3];
} fire_heif_track_info;

/* A decoded image, held by the wrapper until fire_heif_frame_free. */
typedef struct fire_heif_frame {
    uint32_t width;               /* as decoded */
    uint32_t height;
    uint32_t duration;            /* in the track's timescale; 0 for an image item */
    uint8_t bit_depth;            /* luma bits */
    uint8_t has_alpha;
    uint8_t premultiplied;
    uint8_t is_16bit;             /* fire_heif_frame_read writes RGBA16 (else RGBA8) */
    void* image;                  /* opaque */
} fire_heif_frame;

typedef struct fire_heif fire_heif;              /* an opened file; its bytes are borrowed */
typedef struct fire_heif_track fire_heif_track;  /* a sequence track of an opened file */

uint32_t fire_heif_abi_version(void);

/* Open a HEIF / HEIC / AVIF file from bytes in memory. The bytes are borrowed, unmoved and
 * unmodified, until fire_heif_free. NULL on failure, with *out_status saying why. */
fire_heif* fire_heif_open(const uint8_t* data, size_t len, const fire_heif_limits* limits,
                          int* out_status);
void fire_heif_free(fire_heif* file);

/* The primary item's ID, or FIRE_HEIF_NO_IMAGE for a sequence-only file. */
int fire_heif_primary_id(const fire_heif* file, uint32_t* out_id);
int fire_heif_item_info(fire_heif* file, uint32_t item_id, fire_heif_info* out_info);
/* The item's embedded ICC profile, of fire_heif_info::icc_len bytes. */
int fire_heif_item_icc(fire_heif* file, uint32_t item_id, uint8_t* out, size_t len);
/* Decode an image item. Its size is checked against the file's limits first. */
int fire_heif_decode_item(fire_heif* file, uint32_t item_id, const fire_heif_ctl* ctl,
                          fire_heif_frame* out_frame);

/* The ID of `item_id`'s auxiliary image whose type URN is `urn`, e.g. Apple's gain map. */
int fire_heif_aux_find(fire_heif* file, uint32_t item_id, const char* urn, uint32_t* out_aux_id);
int fire_heif_aux_info(fire_heif* file, uint32_t item_id, uint32_t aux_id, fire_heif_info* out_info);
int fire_heif_decode_aux(fire_heif* file, uint32_t item_id, uint32_t aux_id,
                         const fire_heif_ctl* ctl, fire_heif_frame* out_frame);

/* An item's raw data, e.g. a tmap item's ISO 21496-1 metadata. With `out` NULL, only *out_len is
 * set; otherwise `len` must be at least that. */
int fire_heif_item_data(fire_heif* file, uint32_t item_id, uint8_t* out, size_t len, size_t* out_len);

/* The file's first visual track (video or image sequence), or NULL with *out_status
 * FIRE_HEIF_NO_IMAGE. Freed before its file. */
fire_heif_track* fire_heif_track_open(fire_heif* file, int* out_status);
int fire_heif_track_info_get(const fire_heif_track* track, fire_heif_track_info* out_info);
/* Decode the next frame, on the media timeline (the edit list ignored, so an infinite loop ends,
 * FD18). FIRE_HEIF_END after the last. */
int fire_heif_track_next(fire_heif_track* track, const fire_heif_ctl* ctl, fire_heif_frame* out_frame);
void fire_heif_track_free(fire_heif_track* track);

/* Copy a frame into `out` as interleaved RGBA, tightly packed: RGBA8, or native-endian RGBA16
 * (scaled to 0..65535 by bit replication) when frame->is_16bit. Premultiplied alpha is
 * un-premultiplied. Uses up to `threads` threads, the calling thread included. */
int fire_heif_frame_read(const fire_heif_frame* frame, void* out, size_t len, uint32_t threads);
void fire_heif_frame_free(fire_heif_frame* frame);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* FIRE_HEIF_WRAPPER_H */
