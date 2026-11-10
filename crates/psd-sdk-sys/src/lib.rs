//! FFI bindings to psd_sdk (Molecular Matters, C++) through a thin `extern "C"` C-ABI wrapper
//! (`wrapper.h` / `wrapper.cpp`), plus a small safe Rust API that decodes a PSD's merged image.
//!
//! The wrapper is the merge of fire's and 3D Review's (fire-decode plan, FD3), and the decode is
//! staged so the header is read and **checked before anything is sized from it**:
//! [`decode_psd`] opens the header, has the wrapper check it against the caller's [`Limits`],
//! and only then lets psd_sdk allocate and decode the planes. The wrapper catches its own
//! exceptions at the boundary.
//!
//! The image comes back at the document's own depth: 8-bit, 16-bit, or 32-bit linear float.
//! Narrowing it is the caller's choice, not this crate's.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

// The whole of wrapper.h, including the status codes nothing here returns on its own.
#[allow(dead_code)]
#[path = "bindings.rs"]
mod ffi;

use std::marker::PhantomData;
use std::ptr::NonNull;

/// psd_sdk's colour modes, as [`PsdImage::color_mode`] reports them.
pub mod color_mode {
    pub const BITMAP: u16 = 0;
    pub const GRAYSCALE: u16 = 1;
    pub const INDEXED: u16 = 2;
    pub const RGB: u16 = 3;
    pub const CMYK: u16 = 4;
    pub const MULTICHANNEL: u16 = 7;
    pub const DUOTONE: u16 = 8;
    pub const LAB: u16 = 9;
}

/// What a decode may allocate (FD5's per-setup limits). The wrapper checks the header against
/// these before psd_sdk sizes a single plane.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Per axis.
    pub max_dim: u32,
    /// The interleaved RGBA output, at the document's depth.
    pub max_output_bytes: u64,
    /// The planes psd_sdk holds while decoding: every channel, at the document's depth.
    pub max_planar_bytes: u64,
    /// Photoshop's own limit is 56.
    pub max_channels: u16,
}

/// A decoded PSD merged image: interleaved RGBA at the document's own depth.
#[derive(Debug, Clone)]
pub struct PsdImage {
    pub width: u32,
    pub height: u32,
    /// The channels the composite is drawn from, after colour conversion: 1 grey, 2 grey + alpha,
    /// 3 colour, 4 colour + alpha. **Not** the document's plane count: spot channels and saved
    /// selections past the transparency are never read, so they don't count.
    pub channels: u16,
    /// Source bits per channel: 8, 16 or 32. Also the layout of [`Self::rgba`].
    pub bits_per_channel: u16,
    /// The document's colour mode ([`color_mode`]). Everything but greyscale is converted to RGB.
    pub color_mode: u16,
    /// Interleaved RGBA, row-major, in the source depth: `u8`, native-endian `u16`, or
    /// native-endian `f32` (linear, unclamped) for 8-, 16- and 32-bit documents.
    pub rgba: Vec<u8>,
}

/// Why a PSD decode failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsdError {
    /// The header or the section table could not be read, or a section runs past the file.
    OpenFailed,
    /// A depth, colour mode or channel count the merged read cannot draw.
    Unsupported(String),
    /// Past the caller's [`Limits`]; nothing was allocated.
    TooLarge { width: u32, height: u32 },
    /// No merged image: the PSD was saved without "Maximize Compatibility".
    NoMergedImage,
    /// The file ends before the data its header describes.
    Truncated,
    /// The merged image's data is malformed, e.g. a corrupt RLE stream.
    DecodeFailed,
    /// An indexed document without its 256-colour table.
    NoPalette,
    /// The output buffer could not be allocated.
    OutOfMemory { bytes: usize },
    /// The wrapper returned a status nothing here expects, or was built for another ABI.
    Ffi(i32),
}

impl std::fmt::Display for PsdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PsdError::OpenFailed => write!(f, "psd_sdk could not read the PSD header"),
            PsdError::Unsupported(what) => write!(f, "PSD {what} is not supported"),
            PsdError::TooLarge { width, height } => {
                write!(f, "PSD {width}x{height} is past the decode limits")
            }
            PsdError::NoMergedImage => write!(
                f,
                "PSD has no composited image (re-save with Maximize Compatibility)"
            ),
            PsdError::Truncated => write!(f, "PSD file is truncated"),
            PsdError::DecodeFailed => write!(f, "PSD merged image data is corrupt"),
            PsdError::NoPalette => write!(f, "indexed PSD has no colour table"),
            PsdError::OutOfMemory { bytes } => {
                write!(f, "not enough memory to decode the PSD ({bytes} bytes)")
            }
            PsdError::Ffi(code) => write!(f, "PSD decode failed (wrapper status {code})"),
        }
    }
}

impl std::error::Error for PsdError {}

/// An open document, freed on drop. It reads the caller's bytes in place rather than copying the
/// file, so it borrows them: the lifetime makes the compiler hold the slice alive, unmoved and
/// unmodified, for as long as the handle exists.
struct Doc<'bytes> {
    handle: NonNull<ffi::fire_psd>,
    _bytes: PhantomData<&'bytes [u8]>,
}

impl<'bytes> Doc<'bytes> {
    fn open(bytes: &'bytes [u8]) -> Option<Self> {
        // SAFETY: `bytes` is a valid slice of `bytes.len()` bytes, and the returned handle borrows
        // it for `'bytes`, which `Doc` carries: the slice outlives every use of the handle.
        let handle = unsafe { ffi::fire_psd_open(bytes.as_ptr(), bytes.len()) };
        NonNull::new(handle).map(|handle| Self {
            handle,
            _bytes: PhantomData,
        })
    }
}

impl Drop for Doc<'_> {
    fn drop(&mut self) {
        // SAFETY: the handle came from fire_psd_open and is freed exactly once, here.
        unsafe { ffi::fire_psd_free(self.handle.as_ptr()) };
    }
}

/// What an `Unsupported` status refers to, from the header.
fn describe_unsupported(info: &ffi::fire_psd_info) -> String {
    let mode_name = match info.color_mode {
        color_mode::BITMAP => "Bitmap",
        color_mode::GRAYSCALE => "Grayscale",
        color_mode::INDEXED => "Indexed",
        color_mode::RGB => "RGB",
        color_mode::CMYK => "CMYK",
        color_mode::MULTICHANNEL => "Multichannel",
        color_mode::DUOTONE => "Duotone",
        color_mode::LAB => "Lab",
        other => return format!("color mode {other}"),
    };
    if !matches!(info.bits_per_channel, 8 | 16 | 32) {
        return format!("{}-bit {mode_name} document", info.bits_per_channel);
    }
    format!("{mode_name} document with {} channels", info.channels)
}

fn status_error(status: i32, info: &ffi::fire_psd_info) -> PsdError {
    match status {
        ffi::FIRE_PSD_UNSUPPORTED => PsdError::Unsupported(describe_unsupported(info)),
        ffi::FIRE_PSD_TOO_LARGE => PsdError::TooLarge {
            width: info.width,
            height: info.height,
        },
        ffi::FIRE_PSD_NO_MERGED_IMAGE => PsdError::NoMergedImage,
        ffi::FIRE_PSD_TRUNCATED => PsdError::Truncated,
        ffi::FIRE_PSD_DECODE_FAILED => PsdError::DecodeFailed,
        ffi::FIRE_PSD_NO_PALETTE => PsdError::NoPalette,
        other => PsdError::Ffi(other),
    }
}

/// Decode a PSD's merged image from in-memory bytes into RGBA at the document's own depth,
/// allocating no more than `limits` allow, with up to `threads` threads (the calling one
/// included).
pub fn decode_psd(bytes: &[u8], limits: &Limits, threads: usize) -> Result<PsdImage, PsdError> {
    // SAFETY: takes no arguments and touches no state.
    let abi = unsafe { ffi::fire_psd_abi_version() };
    if abi != ffi::FIRE_PSD_ABI_VERSION {
        return Err(PsdError::Ffi(-(abi as i32)));
    }
    let doc = Doc::open(bytes).ok_or(PsdError::OpenFailed)?;
    let mut info = ffi::fire_psd_info::default();
    // SAFETY: `doc` is live and `info` is a valid, writable fire_psd_info.
    if unsafe { ffi::fire_psd_info_get(doc.handle.as_ptr(), &mut info) } != ffi::FIRE_PSD_OK {
        return Err(PsdError::OpenFailed);
    }
    let c_limits = ffi::fire_psd_limits {
        max_dim: limits.max_dim.into(),
        max_output_bytes: limits.max_output_bytes,
        max_planar_bytes: limits.max_planar_bytes,
        max_channels: limits.max_channels.into(),
        reserved: 0,
    };
    // SAFETY: `doc` is live and `c_limits` outlives the call.
    let status = unsafe { ffi::fire_psd_decode_merged(doc.handle.as_ptr(), &c_limits) };
    if status != ffi::FIRE_PSD_OK {
        return Err(status_error(status, &info));
    }

    // A successful decode vouches for the depth (8, 16 or 32) and for the size being inside
    // `limits`; the arithmetic is checked anyway, since it sizes an allocation.
    let too_large = PsdError::TooLarge {
        width: info.width,
        height: info.height,
    };
    let len = (info.width as usize)
        .checked_mul(info.height as usize)
        .and_then(|n| n.checked_mul(4 * usize::from(info.bits_per_channel / 8)))
        .filter(|&n| n != 0 && n as u64 <= limits.max_output_bytes)
        .ok_or(too_large)?;
    let mut rgba: Vec<u8> = Vec::new();
    rgba.try_reserve_exact(len)
        .map_err(|_| PsdError::OutOfMemory { bytes: len })?;
    let threads = u32::try_from(threads).unwrap_or(u32::MAX);
    // SAFETY: `doc` is live and decoded; the pointer and `len` describe `rgba`'s reserved,
    // writable capacity.
    let status = unsafe {
        ffi::fire_psd_read_merged(
            doc.handle.as_ptr(),
            rgba.spare_capacity_mut().as_mut_ptr().cast(),
            len,
            threads,
        )
    };
    if status != ffi::FIRE_PSD_OK {
        return Err(status_error(status, &info));
    }
    // SAFETY: on FIRE_PSD_OK the wrapper has written all `len` bytes: four samples of the
    // document's depth for every pixel, which is exactly `len` (wrapper.h).
    unsafe { rgba.set_len(len) };

    let grey = matches!(
        info.color_mode,
        color_mode::GRAYSCALE | color_mode::DUOTONE | color_mode::BITMAP
    );
    Ok(PsdImage {
        width: info.width,
        height: info.height,
        channels: if grey { 1 } else { 3 } + info.has_alpha.min(1),
        bits_per_channel: info.bits_per_channel,
        color_mode: info.color_mode,
        rgba,
    })
}
