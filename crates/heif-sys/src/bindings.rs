// Hand-kept bindings to `wrapper.h`, ABI version 2 (FD16): no bindgen or libclang at build time.
// When `wrapper.h` changes, change this file to match. The layout asserts below hold the Rust side
// to the sizes and offsets `wrapper.cpp` asserts too.

use std::os::raw::{c_char, c_int, c_void};

pub const FIRE_HEIF_ABI_VERSION: u32 = 2;

pub const FIRE_HEIF_OK: c_int = 0;
pub const FIRE_HEIF_BAD_ARGUMENT: c_int = -1;
pub const FIRE_HEIF_TOO_LARGE: c_int = -2;
pub const FIRE_HEIF_CANCELLED: c_int = -3;
pub const FIRE_HEIF_NO_IMAGE: c_int = -4;
pub const FIRE_HEIF_BAD_IMAGE: c_int = -5;
pub const FIRE_HEIF_EXCEPTION: c_int = -6;
pub const FIRE_HEIF_BUFFER_TOO_SMALL: c_int = -7;
pub const FIRE_HEIF_NOT_FOUND: c_int = -8;
pub const FIRE_HEIF_END: c_int = -9;
pub const FIRE_HEIF_INIT_FAILED: c_int = -10;

pub const REPETITIONS_INFINITE: u32 = u32::MAX;

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct fire_heif_limits {
    pub max_dim: u64,
    pub max_output_bytes: u64,
    pub max_total_memory: u64,
    pub max_tiles: u32,
    pub max_sequence_frames: u32,
    pub max_color_profile_size: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_heif_ctl {
    pub library_threads: u32,
    pub codec_threads: u32,
    pub ignore_transformations: u8,
    pub keep_source_color: u8,
    pub force_16bit: u8,
    pub reserved: u8,
    pub reserved2: u32,
    pub user: *mut c_void,
    pub progress: Option<unsafe extern "C" fn(user: *mut c_void, done: u32, total: u32)>,
    pub cancel: Option<unsafe extern "C" fn(user: *mut c_void) -> c_int>,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct fire_heif_info {
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub icc_len: u32,
    pub item_id: u32,
    pub bit_depth: u8,
    pub has_alpha: u8,
    pub premultiplied: u8,
    pub has_nclx: u8,
    pub colour_primaries: u16,
    pub transfer_characteristics: u16,
    pub matrix_coefficients: u16,
    pub full_range: u8,
    pub reserved: u8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct fire_heif_track_info {
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
    pub repetitions: u32,
    pub has_alpha: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_heif_frame {
    pub width: u32,
    pub height: u32,
    pub duration: u32,
    pub bit_depth: u8,
    pub has_alpha: u8,
    pub premultiplied: u8,
    pub is_16bit: u8,
    pub image: *mut c_void,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_heif {
    _unused: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_heif_track {
    _unused: [u8; 0],
}

const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<fire_heif_limits>() == 40);
    assert!(offset_of!(fire_heif_limits, max_tiles) == 24);
    assert!(size_of::<fire_heif_ctl>() == 40);
    assert!(offset_of!(fire_heif_ctl, ignore_transformations) == 8);
    assert!(offset_of!(fire_heif_ctl, user) == 16);
    assert!(offset_of!(fire_heif_ctl, progress) == 24);
    assert!(offset_of!(fire_heif_ctl, cancel) == 32);
    assert!(size_of::<fire_heif_info>() == 36);
    assert!(offset_of!(fire_heif_info, bit_depth) == 24);
    assert!(offset_of!(fire_heif_info, colour_primaries) == 28);
    assert!(offset_of!(fire_heif_info, full_range) == 34);
    assert!(size_of::<fire_heif_track_info>() == 20);
    assert!(size_of::<fire_heif_frame>() == 24);
    assert!(offset_of!(fire_heif_frame, image) == 16);
};

unsafe extern "C" {
    pub fn fire_heif_abi_version() -> u32;
    pub fn fire_heif_open(
        data: *const u8,
        len: usize,
        limits: *const fire_heif_limits,
        out_status: *mut c_int,
    ) -> *mut fire_heif;
    pub fn fire_heif_free(file: *mut fire_heif);
    pub fn fire_heif_primary_id(file: *const fire_heif, out_id: *mut u32) -> c_int;
    pub fn fire_heif_item_info(
        file: *mut fire_heif,
        item_id: u32,
        out_info: *mut fire_heif_info,
    ) -> c_int;
    pub fn fire_heif_item_icc(
        file: *mut fire_heif,
        item_id: u32,
        out: *mut u8,
        len: usize,
    ) -> c_int;
    pub fn fire_heif_decode_item(
        file: *mut fire_heif,
        item_id: u32,
        ctl: *const fire_heif_ctl,
        out_frame: *mut fire_heif_frame,
    ) -> c_int;
    pub fn fire_heif_aux_find(
        file: *mut fire_heif,
        item_id: u32,
        urn: *const c_char,
        out_aux_id: *mut u32,
    ) -> c_int;
    pub fn fire_heif_aux_info(
        file: *mut fire_heif,
        item_id: u32,
        aux_id: u32,
        out_info: *mut fire_heif_info,
    ) -> c_int;
    pub fn fire_heif_decode_aux(
        file: *mut fire_heif,
        item_id: u32,
        aux_id: u32,
        ctl: *const fire_heif_ctl,
        out_frame: *mut fire_heif_frame,
    ) -> c_int;
    pub fn fire_heif_item_data(
        file: *mut fire_heif,
        item_id: u32,
        out: *mut u8,
        len: usize,
        out_len: *mut usize,
    ) -> c_int;
    pub fn fire_heif_track_open(
        file: *mut fire_heif,
        out_status: *mut c_int,
    ) -> *mut fire_heif_track;
    pub fn fire_heif_track_info_get(
        track: *const fire_heif_track,
        out_info: *mut fire_heif_track_info,
    ) -> c_int;
    pub fn fire_heif_track_next(
        track: *mut fire_heif_track,
        ctl: *const fire_heif_ctl,
        out_frame: *mut fire_heif_frame,
    ) -> c_int;
    pub fn fire_heif_track_free(track: *mut fire_heif_track);
    pub fn fire_heif_frame_read(
        frame: *const fire_heif_frame,
        out: *mut c_void,
        len: usize,
        threads: u32,
    ) -> c_int;
    pub fn fire_heif_frame_free(frame: *mut fire_heif_frame);
}
