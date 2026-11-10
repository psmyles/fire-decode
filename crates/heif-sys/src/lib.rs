//! FFI bindings to libheif (HEIC / HEIF via libde265, AVIF via dav1d) through a C-ABI wrapper
//! (`wrapper.h` / `wrapper.cpp`, ABI version 2), plus a small safe Rust API (fire-decode plan,
//! FD16).
//!
//! - [`HeifFile::open`] reads a file from borrowed bytes, with libheif's security limits set from
//!   the caller's [`Limits`].
//! - Its items are decoded with a [`Ctl`]: thread counts, progress and cancel, whether to apply
//!   `irot` / `imir` / `clap`, and whether to keep the source's colour.
//! - A sequence's first visual track is decoded frame by frame through [`Track`].
//!
//! Pixels land once, in a buffer this crate allocates (fallibly, inside the limits): libheif's
//! decoded image is copied straight into it, with bit replication to 16 bits and premultiplied
//! alpha straightened on the way.
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

// The whole of wrapper.h, including what only later migration steps use.
#[allow(dead_code)]
#[path = "bindings.rs"]
mod ffi;

// A named compile_error! on a target this crate has no libraries for (written by build.rs).
include!(concat!(env!("OUT_DIR"), "/target_check.rs"));

use std::ffi::CString;
use std::marker::PhantomData;
use std::os::raw::{c_int, c_void};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

pub use ffi::REPETITIONS_INFINITE;

/// Apple's HDR gain map, as an auxiliary image type (FD17).
pub const APPLE_GAIN_MAP_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";

/// What a file may make libheif allocate (FD5's per-setup limits). 0 leaves libheif's default.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    /// Per axis, checked against an item's signalled size before it is decoded.
    pub max_dim: u32,
    /// The decoded RGBA, at its depth. Also caps libheif's image area.
    pub max_output_bytes: u64,
    /// libheif's own memory budget for one file.
    pub max_total_memory: u64,
    pub max_tiles: u32,
    pub max_sequence_frames: u32,
    pub max_color_profile_size: u32,
}

/// How to run a decode.
#[derive(Clone, Copy, Default)]
pub struct Ctl<'a> {
    /// The thread budget (FD8). With `codec_threads` 0 the wrapper splits it by tiling: a grid's
    /// tiles in parallel, a single image on the codec's own threads.
    pub library_threads: usize,
    pub codec_threads: usize,
    /// Leave `irot` / `imir` / `clap` unapplied: gain-map compositing applies them afterwards
    /// (FD17).
    pub ignore_transformations: bool,
    /// Keep the source's nclx colour (HDR, FD17); otherwise libheif converts to sRGB.
    pub keep_source_color: bool,
    /// RGBA16 even from an 8-bit source.
    pub force_16bit: bool,
    /// Called with a fraction in `0.0..=1.0` that only rises, possibly from libheif's threads.
    pub progress: Option<&'a (dyn Fn(f32) + Sync)>,
    /// Polled during the decode; `true` stops it with [`HeifError::Cancelled`].
    pub cancel: Option<&'a (dyn Fn() -> bool + Sync)>,
}

impl std::fmt::Debug for Ctl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctl")
            .field("library_threads", &self.library_threads)
            .field("codec_threads", &self.codec_threads)
            .field("ignore_transformations", &self.ignore_transformations)
            .field("keep_source_color", &self.keep_source_color)
            .field("force_16bit", &self.force_16bit)
            .field("progress", &self.progress.is_some())
            .field("cancel", &self.cancel.is_some())
            .finish()
    }
}

/// An nclx colour description (ITU-T H.273 code points).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nclx {
    pub colour_primaries: u16,
    pub transfer_characteristics: u16,
    pub matrix_coefficients: u16,
    pub full_range: bool,
}

/// An image item, before decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemInfo {
    pub id: u32,
    /// As shown, after `irot` / `imir` / `clap`. Signalled by the file, not yet decoded.
    pub width: u32,
    pub height: u32,
    /// As stored (`ispe`).
    pub coded_width: u32,
    pub coded_height: u32,
    /// Luma bits: 8, 10 or 12.
    pub bit_depth: u8,
    pub has_alpha: bool,
    pub premultiplied: bool,
    pub nclx: Option<Nclx>,
    /// Bytes of embedded ICC profile; 0 if none.
    pub icc_len: usize,
}

/// A sequence's first visual track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackInfo {
    pub width: u32,
    pub height: u32,
    /// Ticks a second, for [`Image::duration`].
    pub timescale: u32,
    /// How many times to play it; [`REPETITIONS_INFINITE`] for a loop.
    pub repetitions: u32,
    pub has_alpha: bool,
}

/// A decoded image: interleaved RGBA, straight alpha, row-major.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// The source's bits per channel (8 / 10 / 12).
    pub bit_depth: u8,
    pub has_alpha: bool,
    /// `pixels` is native-endian RGBA16 scaled to 0..65535 (else RGBA8).
    pub is_16bit: bool,
    /// For a sequence frame, its duration in the track's timescale; 0 for an item.
    pub duration: u32,
    pub pixels: Vec<u8>,
}

/// Why a HEIF operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeifError {
    /// Past the caller's [`Limits`] (or libheif's), refused before allocating.
    TooLarge,
    Cancelled,
    /// Neither a primary image nor a visual track.
    NoImage,
    NotFound,
    /// libheif's error code.
    Libheif(i32),
    /// libheif returned an image without a usable plane.
    BadImage,
    /// A C++ exception, caught at the boundary.
    Exception,
    /// heif_init failed: no decoders registered.
    InitFailed,
    OutOfMemory {
        bytes: usize,
    },
    /// The wrapper was built for another ABI, or returned something unexpected.
    Ffi(i32),
}

impl std::fmt::Display for HeifError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeifError::TooLarge => write!(f, "HEIF image is past the decode limits"),
            HeifError::Cancelled => write!(f, "HEIF decode cancelled"),
            HeifError::NoImage => write!(f, "HEIF file has no image or image sequence"),
            HeifError::NotFound => write!(f, "HEIF item not found"),
            HeifError::Libheif(code) => {
                write!(f, "libheif failed to decode the image (code {code})")
            }
            HeifError::BadImage => write!(f, "libheif returned an unusable image"),
            HeifError::Exception => write!(f, "libheif raised an exception"),
            HeifError::InitFailed => write!(f, "libheif could not be initialised"),
            HeifError::OutOfMemory { bytes } => {
                write!(
                    f,
                    "not enough memory to decode the HEIF image ({bytes} bytes)"
                )
            }
            HeifError::Ffi(code) => write!(f, "HEIF wrapper error (status {code})"),
        }
    }
}

impl std::error::Error for HeifError {}

fn check(status: c_int) -> Result<(), HeifError> {
    match status {
        ffi::FIRE_HEIF_OK => Ok(()),
        ffi::FIRE_HEIF_TOO_LARGE => Err(HeifError::TooLarge),
        ffi::FIRE_HEIF_CANCELLED => Err(HeifError::Cancelled),
        ffi::FIRE_HEIF_NO_IMAGE => Err(HeifError::NoImage),
        ffi::FIRE_HEIF_NOT_FOUND => Err(HeifError::NotFound),
        ffi::FIRE_HEIF_BAD_IMAGE => Err(HeifError::BadImage),
        ffi::FIRE_HEIF_EXCEPTION => Err(HeifError::Exception),
        ffi::FIRE_HEIF_INIT_FAILED => Err(HeifError::InitFailed),
        code if code > 0 => Err(HeifError::Libheif(code)),
        code => Err(HeifError::Ffi(code)),
    }
}

/// The caller's closures behind the wrapper's C callbacks, plus the highest progress reported, so
/// what the caller sees only rises (FD16).
struct Callbacks<'a> {
    progress: Option<&'a (dyn Fn(f32) + Sync)>,
    cancel: Option<&'a (dyn Fn() -> bool + Sync)>,
    reported: AtomicU32,
}

unsafe extern "C" fn progress_trampoline(user: *mut c_void, done: u32, total: u32) {
    // SAFETY: `user` is the `Callbacks` that `with_ctl` lent the wrapper for the call; it outlives
    // every callback, and is only read (its atomic is the one shared state).
    let callbacks = unsafe { &*(user as *const Callbacks<'_>) };
    let Some(progress) = callbacks.progress else {
        return;
    };
    if total == 0 {
        return;
    }
    let fraction = (done as f32 / total as f32).clamp(0.0, 1.0);
    let bits = fraction.to_bits();
    // Only a rise is reported. Positive f32s order like their bit patterns.
    if callbacks.reported.fetch_max(bits, Ordering::Relaxed) < bits {
        // A panic must not unwind into C++: drop it, and the decode carries on.
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| progress(fraction)));
    }
}

unsafe extern "C" fn cancel_trampoline(user: *mut c_void) -> c_int {
    // SAFETY: as for `progress_trampoline`.
    let callbacks = unsafe { &*(user as *const Callbacks<'_>) };
    let Some(cancel) = callbacks.cancel else {
        return 0;
    };
    std::panic::catch_unwind(AssertUnwindSafe(cancel)).unwrap_or(true) as c_int
}

/// Run `f` with the wrapper's form of `ctl`, its callbacks pointing at a `Callbacks` that lives
/// for the call.
fn with_ctl<R>(ctl: &Ctl<'_>, f: impl FnOnce(&ffi::fire_heif_ctl) -> R) -> R {
    let callbacks = Callbacks {
        progress: ctl.progress,
        cancel: ctl.cancel,
        reported: AtomicU32::new(0),
    };
    let progress: unsafe extern "C" fn(*mut c_void, u32, u32) = progress_trampoline;
    let cancel: unsafe extern "C" fn(*mut c_void) -> c_int = cancel_trampoline;
    let c = ffi::fire_heif_ctl {
        library_threads: u32::try_from(ctl.library_threads).unwrap_or(u32::MAX),
        codec_threads: u32::try_from(ctl.codec_threads).unwrap_or(u32::MAX),
        ignore_transformations: ctl.ignore_transformations.into(),
        keep_source_color: ctl.keep_source_color.into(),
        force_16bit: ctl.force_16bit.into(),
        reserved: 0,
        reserved2: 0,
        user: &callbacks as *const Callbacks<'_> as *mut c_void,
        progress: ctl.progress.map(|_| progress),
        cancel: ctl.cancel.map(|_| cancel),
    };
    f(&c)
}

/// A decoded frame held by the wrapper, released on drop.
struct Frame(ffi::fire_heif_frame);

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: the frame is empty or came from a successful decode, and is released exactly
        // once, here (releasing an empty frame is a no-op).
        unsafe { ffi::fire_heif_frame_free(&mut self.0) };
    }
}

impl Frame {
    fn empty() -> Self {
        Self(ffi::fire_heif_frame {
            width: 0,
            height: 0,
            duration: 0,
            bit_depth: 0,
            has_alpha: 0,
            premultiplied: 0,
            is_16bit: 0,
            image: std::ptr::null_mut(),
        })
    }

    /// Copy the frame into a buffer of its own, sized from the *decoded* dimensions and checked
    /// against `max_bytes` (FD16: the hard bound, after the signalled size's early refusal).
    fn into_image(self, max_bytes: u64, threads: usize) -> Result<Image, HeifError> {
        let f = &self.0;
        let bpp = if f.is_16bit != 0 { 8 } else { 4 };
        let len = (f.width as usize)
            .checked_mul(f.height as usize)
            .and_then(|n| n.checked_mul(bpp))
            .filter(|&n| n != 0)
            .ok_or(HeifError::BadImage)?;
        if max_bytes != 0 && len as u64 > max_bytes {
            return Err(HeifError::TooLarge);
        }
        let mut pixels: Vec<u8> = Vec::new();
        pixels
            .try_reserve_exact(len)
            .map_err(|_| HeifError::OutOfMemory { bytes: len })?;
        let threads = u32::try_from(threads).unwrap_or(u32::MAX);
        // SAFETY: the frame is live, and the pointer and `len` describe `pixels`' reserved,
        // writable capacity.
        check(unsafe {
            ffi::fire_heif_frame_read(
                f,
                pixels.spare_capacity_mut().as_mut_ptr().cast(),
                len,
                threads,
            )
        })?;
        // SAFETY: on FIRE_HEIF_OK the wrapper has written all `len` bytes: every row, tightly
        // packed (wrapper.h).
        unsafe { pixels.set_len(len) };
        Ok(Image {
            width: f.width,
            height: f.height,
            bit_depth: f.bit_depth,
            has_alpha: f.has_alpha != 0,
            is_16bit: f.is_16bit != 0,
            duration: f.duration,
            pixels,
        })
    }
}

fn item_info(info: &ffi::fire_heif_info) -> ItemInfo {
    ItemInfo {
        id: info.item_id,
        width: info.width,
        height: info.height,
        coded_width: info.coded_width,
        coded_height: info.coded_height,
        bit_depth: info.bit_depth,
        has_alpha: info.has_alpha != 0,
        premultiplied: info.premultiplied != 0,
        nclx: (info.has_nclx != 0).then_some(Nclx {
            colour_primaries: info.colour_primaries,
            transfer_characteristics: info.transfer_characteristics,
            matrix_coefficients: info.matrix_coefficients,
            full_range: info.full_range != 0,
        }),
        icc_len: info.icc_len as usize,
    }
}

/// An open HEIF / HEIC / AVIF file. It reads the caller's bytes in place, so it borrows them for
/// `'bytes`.
pub struct HeifFile<'bytes> {
    file: NonNull<ffi::fire_heif>,
    limits: Limits,
    _bytes: PhantomData<&'bytes [u8]>,
}

impl std::fmt::Debug for HeifFile<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeifFile")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Drop for HeifFile<'_> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from fire_heif_open and is freed exactly once, here; every
        // `Track` borrowing this file has been dropped already.
        unsafe { ffi::fire_heif_free(self.file.as_ptr()) };
    }
}

impl<'bytes> HeifFile<'bytes> {
    /// Open a file. libheif's security limits come from `limits` (FD16).
    pub fn open(bytes: &'bytes [u8], limits: &Limits) -> Result<Self, HeifError> {
        // SAFETY: takes no arguments and touches no state.
        let abi = unsafe { ffi::fire_heif_abi_version() };
        if abi != ffi::FIRE_HEIF_ABI_VERSION {
            return Err(HeifError::Ffi(-(abi as i32)));
        }
        let c = ffi::fire_heif_limits {
            max_dim: limits.max_dim.into(),
            max_output_bytes: limits.max_output_bytes,
            max_total_memory: limits.max_total_memory,
            max_tiles: limits.max_tiles,
            max_sequence_frames: limits.max_sequence_frames,
            max_color_profile_size: limits.max_color_profile_size,
            reserved: 0,
        };
        let mut status: c_int = 0;
        // SAFETY: `bytes` is valid for its length and borrowed for `'bytes`, which the returned
        // file carries; `c` and `status` outlive the call.
        let file = unsafe { ffi::fire_heif_open(bytes.as_ptr(), bytes.len(), &c, &mut status) };
        match NonNull::new(file) {
            Some(file) => Ok(Self {
                file,
                limits: *limits,
                _bytes: PhantomData,
            }),
            None => {
                check(status)?;
                Err(HeifError::Ffi(status))
            }
        }
    }

    fn ptr(&self) -> *mut ffi::fire_heif {
        self.file.as_ptr()
    }

    /// The primary item, or `None` for a sequence-only file (FD16).
    pub fn primary_id(&self) -> Option<u32> {
        let mut id = 0u32;
        // SAFETY: the file is live and `id` is a valid out pointer.
        (unsafe { ffi::fire_heif_primary_id(self.ptr(), &mut id) } == ffi::FIRE_HEIF_OK)
            .then_some(id)
    }

    pub fn item_info(&mut self, id: u32) -> Result<ItemInfo, HeifError> {
        let mut info = ffi::fire_heif_info::default();
        // SAFETY: the file is live and `info` is a valid out pointer.
        check(unsafe { ffi::fire_heif_item_info(self.ptr(), id, &mut info) })?;
        Ok(item_info(&info))
    }

    /// The item's embedded ICC profile, if it has one no longer than `max_len`.
    pub fn item_icc(&mut self, info: &ItemInfo, max_len: usize) -> Option<Vec<u8>> {
        if info.icc_len == 0 || info.icc_len > max_len {
            return None;
        }
        let mut icc = vec![0u8; info.icc_len];
        // SAFETY: the file is live and `icc` is a writable buffer of the profile's length.
        let status =
            unsafe { ffi::fire_heif_item_icc(self.ptr(), info.id, icc.as_mut_ptr(), icc.len()) };
        (status == ffi::FIRE_HEIF_OK).then_some(icc)
    }

    /// Decode an image item: its signalled size is checked against the limits first, the decoded
    /// one again before allocating.
    pub fn decode_item(&mut self, id: u32, ctl: &Ctl<'_>) -> Result<Image, HeifError> {
        let mut frame = Frame::empty();
        let file = self.ptr();
        with_ctl(ctl, |c| {
            // SAFETY: the file is live; `c` (and the callbacks it points at) and `frame` outlive
            // the call.
            check(unsafe { ffi::fire_heif_decode_item(file, id, c, &mut frame.0) })
        })?;
        frame.into_image(self.limits.max_output_bytes, ctl.library_threads.max(1))
    }

    /// The ID of `item`'s auxiliary image of type `urn`, e.g. [`APPLE_GAIN_MAP_URN`].
    pub fn aux_find(&mut self, item: u32, urn: &str) -> Result<u32, HeifError> {
        let urn = CString::new(urn).map_err(|_| HeifError::NotFound)?;
        let mut id = 0u32;
        // SAFETY: the file is live, `urn` is NUL-terminated, and `id` is a valid out pointer.
        check(unsafe { ffi::fire_heif_aux_find(self.ptr(), item, urn.as_ptr(), &mut id) })?;
        Ok(id)
    }

    pub fn aux_info(&mut self, item: u32, aux: u32) -> Result<ItemInfo, HeifError> {
        let mut info = ffi::fire_heif_info::default();
        // SAFETY: the file is live and `info` is a valid out pointer.
        check(unsafe { ffi::fire_heif_aux_info(self.ptr(), item, aux, &mut info) })?;
        Ok(item_info(&info))
    }

    pub fn decode_aux(&mut self, item: u32, aux: u32, ctl: &Ctl<'_>) -> Result<Image, HeifError> {
        let mut frame = Frame::empty();
        let file = self.ptr();
        with_ctl(ctl, |c| {
            // SAFETY: as for `decode_item`.
            check(unsafe { ffi::fire_heif_decode_aux(file, item, aux, c, &mut frame.0) })
        })?;
        frame.into_image(self.limits.max_output_bytes, ctl.library_threads.max(1))
    }

    /// An item's raw data, e.g. a `tmap` item's ISO 21496-1 metadata, if no longer than `max_len`.
    pub fn item_data(&mut self, id: u32, max_len: usize) -> Result<Vec<u8>, HeifError> {
        let mut len = 0usize;
        // SAFETY: the file is live; a null `out` asks for the length only.
        check(unsafe {
            ffi::fire_heif_item_data(self.ptr(), id, std::ptr::null_mut(), 0, &mut len)
        })?;
        if len > max_len {
            return Err(HeifError::TooLarge);
        }
        let mut data = vec![0u8; len];
        let mut written = 0usize;
        // SAFETY: the file is live and `data` is a writable buffer of `len` bytes.
        check(unsafe {
            ffi::fire_heif_item_data(self.ptr(), id, data.as_mut_ptr(), data.len(), &mut written)
        })?;
        data.truncate(written);
        Ok(data)
    }

    /// The file's first visual track, for an image sequence.
    pub fn first_track(&mut self) -> Result<Track<'_, 'bytes>, HeifError> {
        let mut status: c_int = 0;
        // SAFETY: the file is live, and outlives the track, which borrows it.
        let track = unsafe { ffi::fire_heif_track_open(self.ptr(), &mut status) };
        match NonNull::new(track) {
            Some(track) => Ok(Track {
                track,
                limits: self.limits,
                _file: PhantomData,
            }),
            None => {
                check(status)?;
                Err(HeifError::NoImage)
            }
        }
    }
}

/// A sequence track of an open file, which it borrows.
pub struct Track<'file, 'bytes> {
    track: NonNull<ffi::fire_heif_track>,
    limits: Limits,
    _file: PhantomData<&'file mut HeifFile<'bytes>>,
}

impl std::fmt::Debug for Track<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Track").finish_non_exhaustive()
    }
}

impl Drop for Track<'_, '_> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from fire_heif_track_open and is freed exactly once, here,
        // while its file is still open (the borrow guarantees it).
        unsafe { ffi::fire_heif_track_free(self.track.as_ptr()) };
    }
}

impl Track<'_, '_> {
    pub fn info(&self) -> Result<TrackInfo, HeifError> {
        let mut info = ffi::fire_heif_track_info::default();
        // SAFETY: the track is live and `info` is a valid out pointer.
        check(unsafe { ffi::fire_heif_track_info_get(self.track.as_ptr(), &mut info) })?;
        Ok(TrackInfo {
            width: info.width,
            height: info.height,
            timescale: info.timescale,
            repetitions: info.repetitions,
            has_alpha: info.has_alpha != 0,
        })
    }

    /// The next frame on the media timeline, or `None` after the last (FD18: the edit list is
    /// ignored, so an infinite loop ends).
    pub fn next_frame(&mut self, ctl: &Ctl<'_>) -> Result<Option<Image>, HeifError> {
        let mut frame = Frame::empty();
        let track = self.track.as_ptr();
        // SAFETY: the track is live; `c` and `frame` outlive the call.
        let status = with_ctl(ctl, |c| unsafe {
            ffi::fire_heif_track_next(track, c, &mut frame.0)
        });
        if status == ffi::FIRE_HEIF_END {
            return Ok(None);
        }
        check(status)?;
        frame
            .into_image(self.limits.max_output_bytes, ctl.library_threads.max(1))
            .map(Some)
    }
}
