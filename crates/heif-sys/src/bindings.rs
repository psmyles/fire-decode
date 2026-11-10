// Hand-kept bindings to `wrapper.h` (FD3, FD16): written once by bindgen 0.71.1, then committed,
// so a build needs no bindgen or libclang. When `wrapper.h` changes, change this file to match.
// The layout asserts below hold the Rust side to the sizes and offsets the C side asserts too.

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct fire_heif_image {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub has_alpha: u8,
    pub is_16bit: u8,
    pub _pad: u8,
    pub pixels: *mut u8,
    pub pixels_len: usize,
    pub icc: *mut u8,
    pub icc_len: usize,
}
#[allow(clippy::unnecessary_operation, clippy::identity_op)]
const _: () = {
    ["Size of fire_heif_image"][::std::mem::size_of::<fire_heif_image>() - 48usize];
    ["Alignment of fire_heif_image"][::std::mem::align_of::<fire_heif_image>() - 8usize];
    ["Offset of field: fire_heif_image::width"]
        [::std::mem::offset_of!(fire_heif_image, width) - 0usize];
    ["Offset of field: fire_heif_image::height"]
        [::std::mem::offset_of!(fire_heif_image, height) - 4usize];
    ["Offset of field: fire_heif_image::bit_depth"]
        [::std::mem::offset_of!(fire_heif_image, bit_depth) - 8usize];
    ["Offset of field: fire_heif_image::has_alpha"]
        [::std::mem::offset_of!(fire_heif_image, has_alpha) - 9usize];
    ["Offset of field: fire_heif_image::is_16bit"]
        [::std::mem::offset_of!(fire_heif_image, is_16bit) - 10usize];
    ["Offset of field: fire_heif_image::_pad"]
        [::std::mem::offset_of!(fire_heif_image, _pad) - 11usize];
    ["Offset of field: fire_heif_image::pixels"]
        [::std::mem::offset_of!(fire_heif_image, pixels) - 16usize];
    ["Offset of field: fire_heif_image::pixels_len"]
        [::std::mem::offset_of!(fire_heif_image, pixels_len) - 24usize];
    ["Offset of field: fire_heif_image::icc"]
        [::std::mem::offset_of!(fire_heif_image, icc) - 32usize];
    ["Offset of field: fire_heif_image::icc_len"]
        [::std::mem::offset_of!(fire_heif_image, icc_len) - 40usize];
};
unsafe extern "C" {
    pub fn fire_heif_decode(
        data: *const u8,
        len: usize,
        out: *mut fire_heif_image,
    ) -> ::std::os::raw::c_int;
}
unsafe extern "C" {
    pub fn fire_heif_image_free(img: *mut fire_heif_image);
}
