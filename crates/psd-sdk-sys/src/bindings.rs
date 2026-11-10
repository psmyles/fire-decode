// Hand-kept bindings to `wrapper.h`, ABI version 2 (FD3): no bindgen or libclang at build time.
// When `wrapper.h` changes, change this file to match. The layout asserts below hold the Rust side
// to the sizes and offsets `wrapper.cpp` asserts too.

use std::os::raw::{c_int, c_void};

pub const FIRE_PSD_ABI_VERSION: u32 = 2;

pub const FIRE_PSD_OK: c_int = 0;
pub const FIRE_PSD_BAD_ARGUMENT: c_int = 1;
pub const FIRE_PSD_UNSUPPORTED: c_int = 2;
pub const FIRE_PSD_TOO_LARGE: c_int = 3;
pub const FIRE_PSD_NO_MERGED_IMAGE: c_int = 4;
pub const FIRE_PSD_TRUNCATED: c_int = 5;
pub const FIRE_PSD_DECODE_FAILED: c_int = 6;
pub const FIRE_PSD_NO_PALETTE: c_int = 7;
pub const FIRE_PSD_NOT_DECODED: c_int = 8;
pub const FIRE_PSD_BUFFER_TOO_SMALL: c_int = 9;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_psd {
    _unused: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct fire_psd_info {
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub bits_per_channel: u16,
    pub color_mode: u16,
    pub has_alpha: u16,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_psd_limits {
    pub max_dim: u64,
    pub max_output_bytes: u64,
    pub max_planar_bytes: u64,
    pub max_channels: u32,
    pub reserved: u32,
}

const _: () = {
    assert!(std::mem::size_of::<fire_psd_info>() == 16);
    assert!(std::mem::align_of::<fire_psd_info>() == 4);
    assert!(std::mem::offset_of!(fire_psd_info, channels) == 8);
    assert!(std::mem::offset_of!(fire_psd_info, bits_per_channel) == 10);
    assert!(std::mem::offset_of!(fire_psd_info, color_mode) == 12);
    assert!(std::mem::offset_of!(fire_psd_info, has_alpha) == 14);
    assert!(std::mem::size_of::<fire_psd_limits>() == 32);
    assert!(std::mem::align_of::<fire_psd_limits>() == 8);
    assert!(std::mem::offset_of!(fire_psd_limits, max_output_bytes) == 8);
    assert!(std::mem::offset_of!(fire_psd_limits, max_planar_bytes) == 16);
    assert!(std::mem::offset_of!(fire_psd_limits, max_channels) == 24);
};

unsafe extern "C" {
    pub fn fire_psd_abi_version() -> u32;
    pub fn fire_psd_open(bytes: *const u8, len: usize) -> *mut fire_psd;
    pub fn fire_psd_info_get(doc: *const fire_psd, out_info: *mut fire_psd_info) -> c_int;
    pub fn fire_psd_decode_merged(doc: *mut fire_psd, limits: *const fire_psd_limits) -> c_int;
    pub fn fire_psd_read_merged(
        doc: *const fire_psd,
        out_pixels: *mut c_void,
        out_len: usize,
        threads: u32,
    ) -> c_int;
    pub fn fire_psd_free(doc: *mut fire_psd);
}
