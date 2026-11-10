// Hand-kept bindings to `wrapper.h` (FD3, FD16): written once by bindgen 0.71.1, then committed,
// so a build needs no bindgen or libclang. When `wrapper.h` changes, change this file to match.
// The layout asserts below hold the Rust side to the sizes and offsets the C side asserts too.

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_psd {
    _unused: [u8; 0],
}
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_psd_info {
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub bits_per_channel: u16,
    pub color_mode: u16,
    pub reserved: u16,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of fire_psd_info"][::std::mem::size_of::<fire_psd_info>() - 16usize];
    ["Alignment of fire_psd_info"][::std::mem::align_of::<fire_psd_info>() - 4usize];
    ["Offset of field: fire_psd_info::width"]
        [::std::mem::offset_of!(fire_psd_info, width) - 0usize];
    ["Offset of field: fire_psd_info::height"]
        [::std::mem::offset_of!(fire_psd_info, height) - 4usize];
    ["Offset of field: fire_psd_info::channels"]
        [::std::mem::offset_of!(fire_psd_info, channels) - 8usize];
    ["Offset of field: fire_psd_info::bits_per_channel"]
        [::std::mem::offset_of!(fire_psd_info, bits_per_channel) - 10usize];
    ["Offset of field: fire_psd_info::color_mode"]
        [::std::mem::offset_of!(fire_psd_info, color_mode) - 12usize];
    ["Offset of field: fire_psd_info::reserved"]
        [::std::mem::offset_of!(fire_psd_info, reserved) - 14usize];
};
unsafe extern "C" {
    pub fn fire_psd_open(bytes: *const u8, len: usize) -> *mut fire_psd;
}
unsafe extern "C" {
    pub fn fire_psd_info_get(
        doc: *const fire_psd,
        out_info: *mut fire_psd_info,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn fire_psd_read_merged(
        doc: *const fire_psd,
        out_pixels: *mut ::std::os::raw::c_void,
        out_len: usize,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn fire_psd_icc_len(doc: *const fire_psd) -> usize;
}
unsafe extern "C" {
    pub fn fire_psd_icc_get(
        doc: *const fire_psd,
        out_icc: *mut u8,
        out_len: usize,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn fire_psd_free(doc: *mut fire_psd);
}
