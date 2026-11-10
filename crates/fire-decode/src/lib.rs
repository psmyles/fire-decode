//! Uniform decode core: `bytes -> (pixels, buffer format, what the file is, optional ICC)`.
//!
//! All format backends live behind the single [`decode`] entry point so callers
//! never see per-format detail. Routing is by magic bytes (and, for camera raw, the file
//! extension, since the many TIFF-structured raws can't be told from plain TIFF by header):
//!   - PSD            -> psd_sdk (C++ FFI) : merged composite, 8/16/32-bit RGBA (+ICC); and
//!     its layers one at a time through [`PsdLayers`] (pure Rust)
//!   - EXR            -> `exr` crate       : half or 32-bit float RGBA, as stored (linear/HDR)
//!   - DDS            -> `ddsfile` + `bcdec_rs`: BC1-BC7 and the uncompressed layouts
//!   - HEIC/HEIF/AVIF -> libheif (C++ FFI) : 8-bit RGBA, or 16-bit RGBA for 10/12-bit (+ICC)
//!   - camera raw     -> [`raw`] preview   : extract the embedded JPEG, decode via zune
//!   - GIF            -> `image` crate     : every frame (animated GIF plays; see [`Animation`])
//!   - Radiance HDR   -> `image` crate     : 32-bit float RGBA (linear/HDR); see [`decode_hdr`]
//!   - PNG            -> `image` crate     : RGBA8/RGBA16 (+ICC); see [`decode_png`]
//!   - zune-supported -> **zune** (hot path): JPEG/BMP/QOI/PPM/WebP/farbfeld/JXL
//!   - else           -> `image` crate     : TIFF/TGA/ICO (formats zune doesn't decode)
//!
//! **Decode speed is the project's primary metric** (time-to-first-pixel), so the common
//! formats run through zune with [`DecoderOptions::new_fast`] (platform intrinsics +
//! unsafe fast paths enabled). zune output is normalized to interleaved RGBA in the
//! source bit depth (8/16/float). The `image` crate is kept as a fallback for the
//! handful of formats zune has no decoder for (TIFF/GIF/TGA), where decode speed is far
//! less important — and, deliberately, for Radiance HDR and PNG, where its decoders
//! measured faster than zune's (and, for HDR, correct where zune-hdr is not); see
//! [`decode_hdr`] / [`decode_png`].
//!
//! Each app asks for a decode through a named setup ([`DecodeOptions::viewer`], `inspect`,
//! `texture`, `thumbnail`; FD5). After the backend, [`decode`] runs the same passes for every
//! format, in this order:
//! 1. **EXIF orientation**, when the setup honours it ([`exif`]): it can swap width and height, so
//!    it goes first, and every dimension reported describes the same upright image;
//! 2. **colour**: an embedded ICC profile, or a wide-gamut nclx description, converted to sRGB
//!    (`icc` feature, [`icc`]);
//! 3. **fit** within the setup's size ([`downscale`]);
//! 4. **narrowing** to the setup's [`Output`] ([`narrow`]), after the fit so a thumbnail narrows
//!    256 px rather than 8K;
//! 5. the opaque-alpha scan, on exactly what is returned.
//!
//! Panics anywhere in a decode come back as [`DecodeError::Panicked`] (FD7), and every parallel
//! pass draws on one process-wide thread budget (FD8, [`budget`]).
//!
//! **Cargo features** (FD13), none on by default: `psd` and `heif` need the native code in
//! `psd-sdk-sys` / `heif-sys`; `icc` brings in lcms2; `raw` decodes camera raw. A format compiled
//! out still routes as before and returns [`DecodeError::Unsupported`], which is what lets CI lint
//! and test the pure-Rust core on its own. `allow-abort` is below.

// The panic guard is `catch_unwind`, which does nothing when panics abort (FD7): a decode panic
// would take the process down. So such a build is refused unless it says it knows.
#[cfg(all(panic = "abort", not(feature = "allow-abort")))]
compile_error!(
    "fire-decode catches decode panics with catch_unwind, which panic = \"abort\" makes inert. \
     Build with panic = \"unwind\", or enable the \"allow-abort\" feature to accept that a decode \
     panic aborts the process."
);

use std::io::Cursor;
use std::path::Path;

mod budget;
mod dds;
mod downscale;
mod exif;
mod format;
#[cfg(feature = "heif")]
mod gainmap;
#[cfg(feature = "heif")]
mod hdr;
mod heif_meta;
#[cfg(feature = "icc")]
mod icc;
#[cfg(feature = "mips")]
pub mod mips;
mod narrow;
mod options;
mod probe;
mod psd_layers;
mod raw;
mod source;
mod tiff;

pub(crate) use budget::{par_chunks, par_chunks_mut};
pub use format::{Format, all_extensions, is_supported_extension, supported_extensions};
pub use options::{DecodeCtl, DecodeOptions, Filter, Fit, Frames, Hdr, Limits, Output, Surfaces};
pub use probe::{Probe, probe};
pub use psd_layers::{BlendMode, LayerCtl, LayerError, LayerImage, LayerInfo, PsdLayers, Rect};
pub use source::{HdrKind, Primaries, Sample, SourceInfo, Transfer};

use options::Progress;

/// Upper bound on the *total* pixel buffer an animated source may hold, across all frames.
/// Every GIF frame is a full RGBA canvas ([`AnimationFrame`]), so memory is
/// `frames × width × height × 4` — bounding the dimensions alone leaves the frame count free to
/// multiply them. 1 GiB is far past any real animation (a 640×480 GIF gets ~870 frames) while
/// keeping a crafted one from exhausting RAM. A setup's own `max_bytes` lowers it further.
const MAX_ANIMATION_BYTES: u64 = 1 << 30; // 1 GiB

/// Hard ceiling on animation frames, independent of [`MAX_ANIMATION_BYTES`]: a tiny canvas
/// makes the byte budget effectively unbounded, and every frame still costs a `Vec` and a
/// timer tick.
const MAX_ANIMATION_FRAMES: usize = 10_000;

/// Reject a header whose declared size would allocate more than the setup's [`Limits`] allow —
/// **before** anything is allocated from it. `bytes_per_pixel` is that of the buffer the caller is
/// about to allocate (i.e. the *normalized RGBA* output: 4, 8, or 16), not the source's own layout.
///
/// This guard is not redundant with the `catch_unwind` that wraps every decode, and cannot be
/// replaced by it: `catch_unwind` catches *panics*, and a `Vec` allocation that fails does not
/// panic — it calls `handle_alloc_error`, which **aborts the process**. A crafted 20-byte header
/// claiming billions of pixels has to be turned away here; nothing downstream can catch it.
///
/// Applied by every backend that sizes a buffer from parsed dimensions *and* can see those
/// dimensions before allocating. PSD and HEIF apply the same limits on the far side of their
/// bridges, before the native code allocates, and again here after the fact, so the shared policy
/// holds even if those guards drift.
fn check_dims(
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    what: &str,
    limits: &Limits,
) -> Result<(), DecodeError> {
    let max_dim = limits.max_dim as usize;
    if width > max_dim || height > max_dim {
        return Err(DecodeError::TooLarge(format!(
            "{what} dimensions {width}x{height} exceed the {max_dim} per-axis decode guard"
        )));
    }
    let bytes = width
        .checked_mul(height)
        .and_then(|px| px.checked_mul(bytes_per_pixel));
    match bytes {
        Some(b) if b as u64 <= limits.max_bytes => Ok(()),
        _ => Err(DecodeError::TooLarge(format!(
            "{what} {width}x{height} needs more than the {}-byte decode guard",
            limits.max_bytes
        ))),
    }
}

/// What a backend needs from the decode it runs in.
pub(crate) struct Cx<'a> {
    pub(crate) opts: &'a DecodeOptions,
    progress: &'a Progress<'a>,
    cancel: &'a (dyn Fn() -> bool + Sync),
}

impl Cx<'_> {
    pub(crate) fn limits(&self) -> &Limits {
        &self.opts.limits
    }

    /// [`check_dims`] against this decode's limits.
    pub(crate) fn check_dims(
        &self,
        width: usize,
        height: usize,
        bytes_per_pixel: usize,
        what: &str,
    ) -> Result<(), DecodeError> {
        check_dims(width, height, bytes_per_pixel, what, &self.opts.limits)
    }

    pub(crate) fn progress(&self, p: f32) {
        self.progress.report(p);
    }

    pub(crate) fn cancelled(&self) -> bool {
        (self.cancel)()
    }

    pub(crate) fn check_cancel(&self) -> Result<(), DecodeError> {
        if self.cancelled() {
            Err(DecodeError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// One scalar slice, reinterpreted as native-endian bytes. `cast_slice` makes this a single
/// memcpy; the per-backend widening loops it replaced were an extra full pass over buffers
/// that can be gigabytes — and there were seven of them.
pub(crate) fn to_ne_bytes<T: bytemuck::Pod>(v: &[T]) -> Vec<u8> {
    bytemuck::cast_slice(v).to_vec()
}

/// Pixel layout of a decoded image. Drives the per-format CPU sampling path and whether the
/// HDR exposure/tonemap path applies (float = HDR, linear working space).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit per channel, sRGB working space (the common LDR case).
    Rgba8Unorm,
    /// 16-bit unsigned per channel, sRGB working space.
    Rgba16Unorm,
    /// 16-bit half-float per channel, linear working space (HDR).
    Rgba16Float,
    /// 32-bit float per channel, linear working space (HDR).
    Rgba32Float,
}

impl PixelFormat {
    /// Whether this format carries HDR/linear data (exposure + tonemap apply).
    pub fn is_hdr(self) -> bool {
        matches!(self, PixelFormat::Rgba16Float | PixelFormat::Rgba32Float)
    }

    /// Bytes per RGBA pixel for this format.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgba8Unorm => 4,
            PixelFormat::Rgba16Unorm | PixelFormat::Rgba16Float => 8,
            PixelFormat::Rgba32Float => 16,
        }
    }

    /// Bits per channel.
    fn bits(self) -> u8 {
        match self {
            PixelFormat::Rgba8Unorm => 8,
            PixelFormat::Rgba16Unorm | PixelFormat::Rgba16Float => 16,
            PixelFormat::Rgba32Float => 32,
        }
    }
}

/// What a multi-surface source's surfaces *are*, for the status bar and for the wording of the
/// viewer's transport controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetKind {
    /// The six faces of a cubemap, in the DDS order `+X -X +Y -Y +Z -Z`.
    CubeFaces,
    /// The layers of a texture array.
    ArrayLayers,
    /// The depth slices of a volume texture.
    VolumeSlices,
}

impl SheetKind {
    /// The word for these surfaces, singular, for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            SheetKind::CubeFaces => "cubemap",
            SheetKind::ArrayLayers => "array",
            SheetKind::VolumeSlices => "volume",
        }
    }
}

/// A source whose one canvas is really several surfaces tiled into a grid.
///
/// A cubemap, a texture array and a volume are all "N images of the same size" — exactly what the
/// viewer's flipbook already displays, so they are composited into one sheet here and handed over
/// with the grid that reads it back. This is *authored* structure, not the guess
/// `flipbook::detect` makes from pixel content, which is why it travels with the image instead of
/// being re-derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetLayout {
    pub cols: u32,
    pub rows: u32,
    /// Surfaces actually present, `1..=cols*rows`. The trailing cells of a partly filled grid are
    /// transparent black and are not meant to be stepped onto.
    pub frames: u32,
    pub kind: SheetKind,
}

/// The dimensions of mip `level` of a `w`x`h` image: each axis halves, flooring but never
/// dropping below 1.
///
/// The one halving rule the whole pipeline agrees on — what the renderer's mip builder steps
/// through, and what a DDS stores its own levels by. It lives here, next to
/// [`DecodedImage::source_mips`], because that field's contract is stated in terms of it and a
/// second copy in the viewer crate would be a rule in two places.
pub fn level_dims(w: u32, h: u32, level: u32) -> (u32, u32) {
    let shift = level.min(31);
    ((w >> shift).max(1), (h >> shift).max(1))
}

/// A successfully decoded image, normalized to RGBA in `format`'s layout.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The buffer's layout. The file's own depth, channels and colour are in [`source`](Self::source).
    pub format: PixelFormat,
    /// What the file is, whatever the buffer was narrowed, rotated or shrunk to (FD4).
    pub source: SourceInfo,
    /// Whether a declared alpha channel (`source.channels` 2 or 4) is entirely opaque — every
    /// sample fully opaque, so there is no transparency to composite. Set by [`decode`] from a scan
    /// of the final buffer; the viewer uses it to skip the default checker backdrop for, e.g., a
    /// screenshot whose alpha is uniformly `0xff`, while still reporting the true format and
    /// letting the user isolate the (all-white) alpha channel. `false` whenever there is no alpha
    /// channel.
    pub alpha_opaque: bool,
    /// Embedded ICC profile bytes, if the backend surfaced one, whether or not it was applied.
    pub icc: Option<Vec<u8>>,
    /// If the image was shrunk to fit [`DecodeOptions::fit`], the original (width, height); the
    /// pixel inspector notes this.
    pub downscaled_from: Option<(u32, u32)>,
    /// The file's own mip chain, levels `1..`, when it brought one and the setup keeps it
    /// ([`DecodeOptions::stored_mips`]). Each buffer is one level in `format`'s layout at
    /// `((width >> i).max(1), (height >> i).max(1))`, smallest last — the same shape and the same
    /// halving rule the viewer's own mip builder uses, so a chain that survives to the worker can
    /// be adopted verbatim instead of rebuilt.
    ///
    /// Only DDS supplies one today, and it is the format where it matters: the levels in a `.dds`
    /// are *authored*, often with a filter and a gamma the box filter does not reproduce, and an
    /// artist opening one wants to see what shipped rather than what we would have computed.
    ///
    /// May be **partial** (a file is free to stop its chain at 4x4) or absent. It is dropped the
    /// moment anything rewrites `pixels` — see [`transform_buffers`](Self::transform_buffers) —
    /// because a chain that no longer describes the canvas is worse than no chain at all.
    pub source_mips: Option<Vec<Vec<u8>>>,
    /// When the canvas is several surfaces tiled into a grid — a cubemap's faces, an array's
    /// layers, a volume's slices — how to read it back. Only DDS sets it, and only with
    /// [`Surfaces::Sheet`]. `None` for the ordinary single-surface case, which is every other
    /// format.
    ///
    /// Unlike a detected sprite-sheet grid this is *authored*: the file says how many surfaces it
    /// holds and how big each one is, so the viewer adopts it rather than offering it as a guess.
    pub layout: Option<SheetLayout>,
    /// Playback timing/pixels for an animated source (animated GIF), with [`Frames::All`]. `None`
    /// for a still image — the common case, so the still path is untouched. When `Some`, `pixels`
    /// above is frame 0 (shown immediately) and [`Animation::frames`] holds the full sequence for
    /// the viewer to cycle through.
    pub animation: Option<Animation>,
}

/// One frame of an animated image: a full, ready-to-display RGBA canvas plus how long to show it.
#[derive(Debug, Clone)]
pub struct AnimationFrame {
    /// Full-canvas RGBA pixels for this frame, already composited over the prior frames by the
    /// decoder (GIF disposal handled), in the parent [`DecodedImage`]'s `format`/dimensions — so
    /// the viewer just swaps the texture with no per-frame compositing.
    pub pixels: Vec<u8>,
    /// How long this frame is displayed before advancing, in milliseconds.
    pub delay_ms: u32,
}

/// Multi-frame animation for an animated source: an animated GIF, or a HEIF / AVIF image sequence
/// with [`Frames::All`] (FD18). Present on a [`DecodedImage`] only when the source has more than
/// one frame.
#[derive(Debug, Clone)]
pub struct Animation {
    /// Every frame in play order (frame 0 included, matching [`DecodedImage::pixels`]). Each is a
    /// complete canvas at the image's dimensions, so playback is a plain texture swap per frame.
    pub frames: Vec<AnimationFrame>,
    /// Whether the sequence was cut short by a cap: 1 GiB of frames (less under a setup's own
    /// `max_bytes`) or 10,000 of them (FD18). The frames kept still play.
    pub truncated: bool,
    /// How many times to play the frames, or `None` to loop forever: a HEIF sequence's
    /// repetitions. A GIF is `None` (the `image` crate doesn't read its loop count), as every
    /// animation was before. Looping is the caller's: the crate decodes the frames once.
    pub loop_count: Option<u32>,
}

impl DecodedImage {
    /// Run one whole-buffer transform over every pixel buffer this image owns — the canvas,
    /// then every animation frame — so the post-decode passes (EXIF orientation, ICC
    /// conversion, fit, narrowing) can never leave `pixels` and [`Animation::frames`] describing
    /// different images: frame 0 duplicates the canvas, and a pass that touched only `pixels`
    /// would desync the moment playback starts. Frames shorter than `needed` bytes (the canvas
    /// size the transform assumes) are dropped rather than indexed past, and an animation left
    /// with no frames is removed entirely.
    pub(crate) fn transform_buffers(&mut self, needed: usize, mut f: impl FnMut(&mut Vec<u8>)) {
        // Every pass that rewrites the canvas comes through here, which makes this the one place
        // that has to invalidate a file-supplied mip chain: a rotated, colour-transformed or
        // resampled level 0 is no longer what those levels are levels *of*. Transforming them
        // alongside would be wrong as often as right — a nearest-neighbour downscale in
        // particular does not produce the file's level 1 — so the chain is dropped and the worker
        // rebuilds one. Orientation 1, no ICC profile and an in-budget image never reach here,
        // which is every ordinary DDS.
        self.source_mips = None;
        f(&mut self.pixels);
        if let Some(anim) = self.animation.as_mut() {
            anim.frames.retain(|frame| frame.pixels.len() >= needed);
            for frame in &mut anim.frames {
                f(&mut frame.pixels);
            }
            if anim.frames.is_empty() {
                self.animation = None;
            }
        }
    }
}

/// Decode failure modes.
///
/// There is deliberately no `UnknownFormat` variant: [`sniff`] always resolves to *some* backend
/// (the `image` crate is the catch-all), so unrecognized input is not a routing failure — it is a
/// backend rejecting bytes it cannot parse, and comes back as [`Malformed`](Self::Malformed).
#[derive(Debug)]
#[non_exhaustive]
pub enum DecodeError {
    /// The backend rejected the data as malformed — including input that is not an image at all.
    Malformed(String),
    /// The image is past the setup's [`Limits`] — a decode bomb, or simply larger than this setup
    /// will allocate for.
    ///
    /// Kept distinct from [`Self::Malformed`] because *refusing* an image and *failing* to read
    /// one call for opposite responses: a decoder that merely failed may be worth retrying with
    /// a different one, and a guard that fired must never be — not by a second decoder in the
    /// same process. (bite retries any failure through ImageMagick, which runs in a process of
    /// its own; FD10.) Conflating them let a 65535x65535 JPEG that zune correctly refused get
    /// handed to the `image` crate, which obligingly allocated it.
    TooLarge(String),
    /// A PSD with no merged image: Photoshop saved it without "Maximize Compatibility". The layers
    /// are all there, but the flattened picture other tools read is not — Photoshop writes a
    /// white placeholder in its place and says so in the Version Info resource, and some writers
    /// leave the section out altogether. Both come back as this.
    ///
    /// Its own variant so a caller can act on it without reading the message: a Unity import takes
    /// the merged image, so Asset Check tells the artist to re-save with Maximize Compatibility.
    NoMergedImage,
    /// A format this build was compiled without (its cargo feature is off).
    Unsupported(Format),
    /// The caller cancelled the decode through [`DecodeCtl::cancel`].
    Cancelled,
    /// The decode panicked. The crate caught it (FD7); the process is fine.
    Panicked,
    /// An FFI backend (psd_sdk, libheif, lcms2) failed; surfaced so the caller survives.
    Ffi(String),
    /// I/O or unexpected backend error.
    Other(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Malformed(m) => write!(f, "malformed image: {m}"),
            DecodeError::TooLarge(m) => write!(f, "image too large: {m}"),
            DecodeError::NoMergedImage => write!(
                f,
                "PSD has no merged image (re-save it with Maximize Compatibility)"
            ),
            DecodeError::Unsupported(format) => match format.feature() {
                Some(feature) => write!(
                    f,
                    "this build has no {} support (compiled without the \"{feature}\" feature)",
                    format.name()
                ),
                None => write!(f, "this build has no {} support", format.name()),
            },
            DecodeError::Cancelled => write!(f, "decode cancelled"),
            DecodeError::Panicked => write!(f, "the decoder panicked"),
            DecodeError::Ffi(m) => write!(f, "decoder FFI error: {m}"),
            DecodeError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Which backend handles a given byte stream.
enum Backend {
    Psd,
    Exr,
    /// HEIC/HEIF/AVIF via libheif; carries which, from the `ftyp` brand.
    Heif(Format),
    /// A zune-decodable format; carries zune's detected format for status-bar naming.
    Zune(zune_image::codecs::ImageFormat),
    /// A camera-raw file; carries the status-bar label for the detected raw family. The
    /// embedded JPEG preview is extracted ([`raw`]) and decoded through the zune path.
    Raw(&'static str),
    /// A GIF (still or animated); decoded frame-by-frame via the `image` crate so an animated
    /// GIF can play. Sniffed separately from [`Backend::Image`] to reach the multi-frame path.
    Gif,
    /// A plain TIFF; decoded against the `tiff` crate directly ([`crate::tiff`]), falling back
    /// to [`Backend::Image`] for the colour types that module does not own.
    Tiff,
    /// Radiance HDR (`.hdr`/`.pic`); decoded by the `image` crate, *not* zune — see
    /// [`decode_hdr`] for why zune-hdr is avoided.
    Hdr,
    /// DDS (DirectDraw Surface); the game-development texture container, decompressed on the
    /// CPU by [`crate::dds`].
    Dds,
    /// A Windows cursor (`.cur`): an ICO container with a different type word, and two of its
    /// directory fields reused for the hotspot. The `image` crate's ICO decoder reads one
    /// unchanged but does not recognize the magic, so it is named for it here.
    Cur,
    /// An uncompressed true-colour TGA, recognized from its header rather than from magic it
    /// does not have — see [`is_uncompressed_tga`], which also keeps it out of [`Backend::Cur`],
    /// whose magic it shares. Decoded by the `image` crate like any other TGA.
    Tga,
    /// PNG; decoded by the `image` crate, *not* zune — see [`decode_png`] for why
    /// zune-png is avoided.
    Png,
    Image,
}

impl Backend {
    /// The format this backend reads, for [`sniff`]. `bytes` and `ext` settle the catch-all.
    fn format(&self, bytes: &[u8], ext: Option<&str>) -> Format {
        match self {
            Backend::Psd => Format::Psd,
            Backend::Exr => Format::Exr,
            Backend::Heif(f) => *f,
            Backend::Zune(z) => zune_format(*z),
            Backend::Raw(_) => Format::CameraRaw,
            Backend::Gif => Format::Gif,
            Backend::Tiff => Format::Tiff,
            Backend::Hdr => Format::RadianceHdr,
            Backend::Dds => Format::Dds,
            Backend::Cur => Format::Cur,
            Backend::Tga => Format::Tga,
            Backend::Png => Format::Png,
            Backend::Image => image::guess_format(bytes)
                .ok()
                .map(image_format)
                .filter(|f| *f != Format::Unknown)
                .or_else(|| ext.and_then(Format::from_extension))
                .unwrap_or(Format::Unknown),
        }
    }
}

/// Detect an ISOBMFF (HEIF-family) stream by its `ftyp` box's major brand. The layout is
/// `[u32 box-size][b"ftyp"][u32 major-brand][...]`, so the major brand sits at bytes 8..12.
/// Returns `None` for non-HEIF input.
fn heif_format(bytes: &[u8]) -> Option<Format> {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    match &bytes[8..12] {
        b"avif" | b"avis" => Some(Format::Avif),
        // HEVC-in-HEIF brands.
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"hevm" | b"hevs" => {
            Some(Format::Heic)
        }
        // Generic HEIF (codec announced in compatible brands; libheif sorts it out).
        b"mif1" | b"msf1" | b"mif2" => Some(Format::Heif),
        _ => None,
    }
}

/// Whether `header` (the first bytes of a `len`-byte file) is an uncompressed true-colour TGA —
/// the variant that wears the Windows cursor's magic.
///
/// `00 00 02 00` is the `.cur` magic (two reserved bytes, then the type word `2`) and it is also,
/// byte for byte, the start of the most ordinary TGA there is: id length 0, no colour map, image
/// type 2 (uncompressed true-colour), then the low byte of the colour-map origin. That is what
/// every art tool writes, so those four bytes cannot decide `Backend::Cur` on their own — they
/// used to, which sent such files to the ICO decoder to be refused ("ICO directory contains no
/// image"). They did not open at all, extension hint or not.
///
/// TGA has no magic to test for (only an *optional* end-of-file `TRUEVISION-XFILE.` footer), so
/// test the header for self-consistency instead: a file with no colour map says so in all three
/// colour-map fields, an uncompressed true-colour depth is one of four values, and the pixel data
/// the dimensions promise has to fit in the file. A cursor cannot satisfy all of that at once —
/// its image count and its first directory entry's width and height land in the very fields this
/// requires to be zero, so passing would mean a cursor holding no images whose first image is
/// 0x0, which has nothing to decode either way.
fn is_uncompressed_tga(header: &[u8], len: usize) -> bool {
    const HEADER: usize = 18;
    let Some(h) = header.get(..HEADER) else {
        return false;
    };
    // Image type 2, and no colour map: type 0 with a zero origin, length and entry size.
    if h[2] != 2 || h[1] != 0 || h[3..8] != [0; 5] {
        return false;
    }
    let width = u16::from_le_bytes([h[12], h[13]]) as usize;
    let height = u16::from_le_bytes([h[14], h[15]]) as usize;
    let depth = h[16];
    if width == 0 || height == 0 || !matches!(depth, 15 | 16 | 24 | 32) {
        return false;
    }
    // Header, then the optional image-id field, then one uncompressed pixel per texel.
    HEADER + h[0] as usize + width * height * (depth as usize).div_ceil(8) <= len
}

/// The backend for a stream whose first bytes are `bytes`, `len` bytes long in all. A few KiB of
/// the start is enough: nothing here looks further in (FD19's [`probe`] routes on a prefix).
fn route(bytes: &[u8], len: usize, ext: Option<&str>) -> Backend {
    use zune_core::bytestream::ZCursor;
    use zune_image::codecs::{ImageFormat, guess_format};

    if bytes.starts_with(b"8BPS") {
        // PSD: psd_sdk gives a better composite than zune-psd, and carries the ICC.
        Backend::Psd
    } else if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        // OpenEXR magic number — handled by the `exr` crate (zune has no EXR decoder).
        Backend::Exr
    } else if let Some(format) = heif_format(bytes) {
        // HEIC/HEIF/AVIF — the ISOBMFF `ftyp` brands; decoded by libheif.
        Backend::Heif(format)
    } else if let Some(label) = raw::label(bytes, ext) {
        // Camera raw (CR2/CR3/NEF/ARW/RAF/DNG/…): display the embedded JPEG preview. Sniffed
        // before the zune/`image` fallback because TIFF-structured raws share TIFF's magic
        // and must not be handed to the `image` crate as ordinary TIFFs.
        Backend::Raw(label)
    } else if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
        // Plain TIFF, decoded against the `tiff` crate directly (see the `tiff` module for why
        // the `image` path loses samples). Sniffed *after* `raw::label` so the many
        // TIFF-structured camera raws, which share this magic, still route to the preview
        // extractor — and it falls back to `image` for the colour types it does not own.
        Backend::Tiff
    } else if bytes.starts_with(b"DDS ") {
        // DDS: a unique four-byte magic, and a container neither zune nor the `image` crate
        // reads the modern formats of (BC4-BC7 and the DX10 header are all past `image`'s
        // DXT-only decoder), so it is sniffed here and handled by our own backend.
        Backend::Dds
    } else if bytes.starts_with(&[0x00, 0x00, 0x02, 0x00]) {
        // Byte 2 is the ICO/CUR type word, 1 for an icon and 2 for a cursor — but a cursor is
        // not the only thing that claims this magic. An uncompressed true-colour TGA opens with
        // the same four bytes, so the header has to break the tie; see `is_uncompressed_tga`.
        if is_uncompressed_tga(bytes, len) {
            Backend::Tga
        } else {
            // A cursor. The ICO decoder reads the payload unchanged.
            Backend::Cur
        }
    } else if bytes.starts_with(b"GIF8") {
        // GIF (b"GIF87a"/b"GIF89a"): route to the dedicated multi-frame decoder so an animated
        // GIF plays. zune has no GIF decoder anyway, so this only pre-empts the `image` fallback.
        Backend::Gif
    } else if let Some((fmt, _)) = guess_format(ZCursor::new(bytes)) {
        // zune recognizes it (JPEG/BMP/QOI/PPM/WebP/farbfeld/JXL): the fast path — except
        // HDR and PNG, which zune sniffs for us but the `image` crate decodes (its decoders
        // measured faster than zune's for both; see decode_hdr / decode_png).
        match fmt {
            ImageFormat::Unknown => Backend::Image,
            // zune's sniff covers both the `#?RADIANCE` and `#?RGBE` magics for us.
            ImageFormat::HDR => Backend::Hdr,
            ImageFormat::PNG => Backend::Png,
            _ => Backend::Zune(fmt),
        }
    } else {
        // zune doesn't sniff it (TIFF/GIF/TGA/ICO): fall back to the image crate.
        Backend::Image
    }
}

/// The format `bytes` hold, by magic bytes, then by `ext_hint` (no leading dot, any case) for
/// formats with no magic (TGA) or that share it (the TIFF-structured camera raws). Whatever this
/// build decodes: a PSD is [`Format::Psd`] without the `psd` feature too. Unrecognised bytes with
/// no telling extension are [`Format::Unknown`]. A few KiB of the file's start is enough.
pub fn sniff(bytes: &[u8], ext_hint: Option<&str>) -> Format {
    route(bytes, bytes.len(), ext_hint).format(bytes, ext_hint)
}

/// Decode an in-memory image, choosing a backend by magic bytes. A panic inside the decode comes
/// back as [`DecodeError::Panicked`] (FD7).
pub fn decode(
    bytes: &[u8],
    ext_hint: Option<&str>,
    opts: &DecodeOptions,
) -> Result<DecodedImage, DecodeError> {
    decode_with_progress(bytes, ext_hint, opts, &DecodeCtl::NONE)
}

/// [`decode`], reporting how far the backend has got through `ctl.progress` and stopping when
/// `ctl.cancel` says so (FD16). See [`DecodeCtl`] for which backends report. The passes after the
/// backend (orientation, colour, fit, narrowing) are not counted.
pub fn decode_with_progress(
    bytes: &[u8],
    ext_hint: Option<&str>,
    opts: &DecodeOptions,
    ctl: &DecodeCtl<'_>,
) -> Result<DecodedImage, DecodeError> {
    guarded(|| {
        // The calling thread is one of the budget's for the whole decode (FD8).
        let _seat = budget::Seat::take();
        let cap = opts.max_threads.map_or(0, |n| n.max(1));
        budget::with_cap(cap, || decode_inner(bytes, ext_hint, opts, ctl))
    })
}

/// Run `f`, turning a panic into [`DecodeError::Panicked`]. The panic's message still goes to the
/// panic hook (stderr by default), which can tell it is a decode's from [`in_decode`].
pub(crate) fn guarded<T>(f: impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _decoding = budget::Decoding::enter();
        f()
    }))
    .unwrap_or(Err(DecodeError::Panicked))
}

/// Whether the calling thread is running a decode: inside [`decode`], [`decode_with_progress`],
/// [`probe`] or [`PsdLayers::decode_layer`], or one of the threads they hand work to. For a panic
/// hook that aborts on every panic but a decode's, which the crate turns into
/// [`DecodeError::Panicked`] (FD7): 3D Review's viewer aborts loudly on its own bugs, and a
/// crafted file is just a file that didn't open. The hook runs before the panic unwinds, so this
/// is still `true` there.
pub fn in_decode() -> bool {
    budget::decoding()
}

fn decode_inner(
    bytes: &[u8],
    ext_hint: Option<&str>,
    opts: &DecodeOptions,
    ctl: &DecodeCtl<'_>,
) -> Result<DecodedImage, DecodeError> {
    if opts.hdr == Hdr::Linear && opts.output == Output::Rgba8 {
        return Err(DecodeError::Other(
            "Hdr::Linear needs an output that keeps float (Native or Rgba8OrFloat), not Rgba8"
                .into(),
        ));
    }
    let progress = Progress::new(ctl.progress);
    let cx = Cx {
        opts,
        progress: &progress,
        cancel: ctl.cancel,
    };
    let backend = route(bytes, bytes.len(), ext_hint);
    // Camera raw resolves its own orientation while walking the container for the preview
    // (`decode_raw`), and the preview it hands back is already upright — reading the file's TIFF
    // directory again below would rotate it a second time.
    let orients_itself = matches!(&backend, Backend::Raw(_));

    let mut img = match backend {
        Backend::Psd => decode_psd(bytes, &cx)?,
        Backend::Exr => decode_exr(bytes, &cx)?,
        Backend::Heif(format) => decode_heif(bytes, format, &cx)?,
        Backend::Raw(label) => decode_raw(bytes, label, &cx)?,
        Backend::Gif => decode_gif(bytes, &cx)?,
        Backend::Tiff => {
            // The `ExtraSamples` fixup has to happen before the decoder is constructed — either
            // decoder — because both decide the sample layout at construction time.
            let patched = tiff::extra_sample_as_alpha(bytes);
            match tiff::decode(&patched, &cx) {
                Some(result) => result?,
                None => decode_image(&patched, ext_hint, &cx)?,
            }
        }
        Backend::Hdr => decode_hdr(bytes, &cx)?,
        Backend::Dds => dds::decode(bytes, &cx)?,
        Backend::Cur => decode_cur(bytes, &cx)?,
        // Sniffed from its header, so it needs no extension hint to name itself.
        Backend::Tga => decode_image(bytes, Some("tga"), &cx)?,
        Backend::Png => decode_png(bytes, &cx)?,
        // zune is the hot path, not the only path. It has gaps the `image` crate does not —
        // zune-bmp cannot read a 24-bit BMP narrower than 3 pixels, so a 1x1 colour swatch
        // failed to open at all — and where the fast decoder simply says no, retrying with the
        // slower one costs nothing (we are already on the error path) and turns "won't open"
        // into "opens". A file both reject still reports zune's error, the more precise one.
        //
        // A guard rejection is emphatically NOT retried: `TooLarge` means we *decided* not to
        // decode this, and handing it to a second decoder would be walking around our own
        // decode-bomb defence.
        Backend::Zune(fmt) => match decode_zune(bytes, fmt, &cx) {
            Ok(img) => img,
            Err(DecodeError::TooLarge(m)) => return Err(DecodeError::TooLarge(m)),
            Err(zune_err) => match decode_image(bytes, ext_hint, &cx) {
                Ok(img) => img,
                // The *fallback's* guard rejections keep their identity too: `TooLarge` means
                // "we decided not to", and must stay distinguishable from "failed to decode"
                // no matter which decoder's guard fired.
                Err(e @ DecodeError::TooLarge(_)) => return Err(e),
                // Both decoders said no: report zune's error, the more precise one.
                Err(_) => return Err(zune_err),
            },
        },
        Backend::Image => decode_image(bytes, ext_hint, &cx)?,
    };
    cx.check_cancel()?;

    // A float buffer is linear light (OpenEXR, Radiance HDR, float TIFF, 32-bit PSD, PFM); say so
    // before narrowing can turn it into bytes. Not a PQ / HLG source decoded to light: its file is
    // still the integer signal its nclx box describes.
    if img.format.is_hdr()
        && !matches!(
            img.source.hdr,
            HdrKind::Pq | HdrKind::Hlg | HdrKind::GainMap
        )
    {
        img.source.sample = Sample::Float;
        if img.source.transfer == Transfer::Srgb {
            img.source.transfer = Transfer::Linear;
        }
    }

    // Rotate to the orientation the file asks for. First, before the colour transform and the
    // fit, because it can swap width/height: doing it here is the only way `downscaled_from` and
    // every dimension the viewer reports describe the same (upright) image. Orientation 1 —
    // almost every file — costs one tag lookup and no pixel work at all.
    if opts.honor_exif && !orients_itself {
        exif::apply(&mut img, exif::orientation(bytes));
    }

    // An embedded ICC profile, or a wide-gamut nclx description with none, into sRGB.
    #[cfg(feature = "icc")]
    if opts.honor_icc {
        icc::apply(&mut img);
        cx.check_cancel()?;
    }

    downscale::apply(&mut img, opts.fit);
    narrow::apply(&mut img, opts.output);
    cx.check_cancel()?;

    // Flag a declared-but-fully-opaque alpha channel. The source's `channels` is left truthful —
    // a 32-bit PNG screenshot still reports RGBA and keeps an inspectable alpha — but the viewer
    // reads this to avoid defaulting to the checker backdrop when there is no actual transparency
    // to reveal. Scanned on the final buffer, i.e. exactly what is returned; the scan
    // short-circuits on the first transparent sample.
    img.alpha_opaque = matches!(img.source.channels, 2 | 4) && alpha_is_opaque(&img);

    Ok(img)
}

/// Whether the normalized RGBA buffer is fully opaque (every alpha sample at its max). Run once
/// per decode off the UI thread (see [`decode`]).
///
/// A scan over just the alpha lane, split across threads and taken a 64 KiB block at a time: each
/// block is checked without an early exit, which is what lets it vectorize, and the scan stops
/// between blocks as soon as any thread has found a transparent sample. On an opaque 8192² RGBA8
/// image — the case that has to read everything — that is ~4 ms against ~25 ms for the
/// sample-at-a-time loop it replaced.
fn alpha_is_opaque(img: &DecodedImage) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};

    fn block_is_opaque(format: PixelFormat, block: &[u8]) -> bool {
        match format {
            // 8-bit: 4 bytes/px, alpha is byte 3; opaque == 0xff.
            PixelFormat::Rgba8Unorm => {
                block.as_chunks::<4>().0.iter().fold(0xff, |m, p| m & p[3]) == 0xff
            }
            // 16-bit unorm (native-endian u16): 8 bytes/px, alpha is bytes 6..8; opaque == 0xffff.
            PixelFormat::Rgba16Unorm => {
                block
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .fold(0xffff, |m, p| m & u16::from_ne_bytes([p[6], p[7]]))
                    == 0xffff
            }
            // 16-bit half-float (half-float OpenEXR, RGBA16F/BC6H DDS): opaque == 1.0 == 0x3c00.
            PixelFormat::Rgba16Float => block.as_chunks::<8>().0.iter().fold(true, |ok, p| {
                ok & (u16::from_ne_bytes([p[6], p[7]]) == 0x3c00)
            }),
            // 32-bit float (linear/HDR): 16 bytes/px, alpha is the 4th f32. Opaque == 1.0; values
            // above 1.0 count as opaque, NaN does not (keeping the alpha channel is the safe
            // default).
            PixelFormat::Rgba32Float => block.as_chunks::<16>().0.iter().fold(true, |ok, p| {
                ok & (f32::from_ne_bytes([p[12], p[13], p[14], p[15]]) >= 1.0)
            }),
        }
    }

    const BLOCK_BYTES: usize = 64 * 1024; // a whole number of pixels at every bytes-per-pixel
    let transparent = AtomicBool::new(false);
    par_chunks(&img.pixels, BLOCK_BYTES, |_, part| {
        for block in part.chunks(BLOCK_BYTES) {
            if transparent.load(Ordering::Relaxed) {
                return;
            }
            if !block_is_opaque(img.format, block) {
                transparent.store(true, Ordering::Relaxed);
                return;
            }
        }
    });
    !transparent.into_inner()
}

/// Convenience wrapper: read a file and decode it.
pub fn decode_path(path: &Path, opts: &DecodeOptions) -> Result<DecodedImage, DecodeError> {
    let bytes = std::fs::read(path).map_err(|e| DecodeError::Other(e.to_string()))?;
    let ext = path.extension().and_then(|e| e.to_str());
    decode(&bytes, ext, opts)
}

/// Whether an ICC profile is sRGB in effect: converting from it to sRGB moves no colour by more
/// than one 8-bit code. That is true of the many sRGB profiles in circulation, whatever their
/// description or byte layout, and false of Display P3, Adobe RGB and the like, whose colours an
/// app that ignores ICC (Unity, for one) shows shifted.
///
/// An RGB profile gets the probe the ICC pass uses to skip sRGB-tagged images (a 17³ lattice plus
/// a full ramp per primary, ~0.1 ms), here with the relative colorimetric intent, because the
/// question is what the profile's colours *are*. A greyscale profile is sRGB when its tone curve
/// is: every grey level lands on the same sRGB grey. Any other colour space, and a profile lcms2
/// cannot parse, is not.
#[cfg(feature = "icc")]
pub fn icc_is_srgb(profile: &[u8]) -> bool {
    icc::is_srgb(profile)
}

// --- backends ---------------------------------------------------------------

/// PSD via psd_sdk (C++ FFI). The C++ side guards against its own exceptions; a Rust-side panic
/// is caught by [`decode`]'s guard.
#[cfg(feature = "psd")]
fn decode_psd(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    // Checked before psd_sdk runs, because psd_sdk cannot tell: the placeholder Photoshop writes
    // is a well-formed image section that decodes to a white canvas. One walk over the image
    // resources, no pixel work.
    if psd_layers::merged_image_is_placeholder(bytes) {
        return Err(DecodeError::NoMergedImage);
    }
    // The wrapper checks the header against the setup's limits before psd_sdk sizes a plane
    // (FD3); the planar cap is twice the output's, and 56 channels is Photoshop's own limit.
    let lim = cx.limits();
    let limits = psd_sdk_sys::Limits {
        max_dim: lim.max_dim,
        max_output_bytes: lim.max_bytes,
        max_planar_bytes: lim.max_bytes.saturating_mul(2),
        max_channels: 56,
    };
    // The bridge's threads come from the budget, held for the whole call (FD8).
    let lease = budget::lease(budget::cores());
    let psd = psd_sdk_sys::decode_psd(bytes, &limits, lease.threads()).map_err(|e| match e {
        psd_sdk_sys::PsdError::NoMergedImage => DecodeError::NoMergedImage,
        e @ psd_sdk_sys::PsdError::TooLarge { .. } => DecodeError::TooLarge(e.to_string()),
        e @ psd_sdk_sys::PsdError::OutOfMemory { .. } => DecodeError::Other(e.to_string()),
        e => DecodeError::Malformed(e.to_string()),
    })?;
    drop(lease);
    // The buffer is already allocated (psd-sdk-sys sizes it, bounded by the C++ guard and
    // its own cap — it cannot wait for us, the dimensions live behind the FFI), so this
    // check_dims is enforcement of the shared policy rather than the allocation guard the
    // other backends use it as: the per-axis cap and byte budget hold for PSD too, even if
    // the guards on the far side of the boundary drift.
    let bps = (psd.bits_per_channel as usize).div_ceil(8);
    cx.check_dims(psd.width as usize, psd.height as usize, 4 * bps, "PSD")?;
    // The wrapper hands back the document's own depth rather than always narrowing to 8-bit:
    // a 16-bit PSD keeps its precision (and a 32-bit one its linear/HDR range) instead of
    // being flattened on the way in.
    let format = match psd.bits_per_channel {
        16 => PixelFormat::Rgba16Unorm,
        32 => PixelFormat::Rgba32Float,
        _ => PixelFormat::Rgba8Unorm,
    };
    // Modes the wrapper converts out of leave the embedded profile describing a space these
    // pixels are no longer in; applying it downstream would convert a second time.
    use psd_sdk_sys::color_mode::{CMYK, LAB, MULTICHANNEL};
    let icc = if matches!(psd.color_mode, CMYK | MULTICHANNEL | LAB) {
        None
    } else {
        psd_layers::composite_icc(bytes)
    };
    Ok(DecodedImage {
        pixels: psd.rgba,
        width: psd.width,
        height: psd.height,
        format,
        source: SourceInfo::new(
            Format::Psd,
            "PSD",
            psd.channels.min(255) as u8,
            psd.bits_per_channel.min(255) as u8,
        ),
        icc,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    })
}

/// Stand-in for [`decode_psd`] when the crate is built without the vendored psd_sdk. Routing
/// deliberately does *not* change with the feature: the bytes really are a PSD, this build just
/// cannot read one, and saying so is more honest than letting a fallback decoder mangle it.
#[cfg(not(feature = "psd"))]
fn decode_psd(_bytes: &[u8], _cx: &Cx) -> Result<DecodedImage, DecodeError> {
    Err(DecodeError::Unsupported(Format::Psd))
}

/// The OpenEXR fast path: read the first layer's channels as the file stores them, then
/// interleave R, G, B and (if present) A on several threads.
///
/// A file whose four channels are all half-float stays half: `Rgba16Float`, which every consumer
/// of a decoded image already handles (BC6H and RGBA16F DDS arrive that way). That is lossless,
/// and it halves everything downstream of the decode — the buffer, the mip chain and the upload
/// (1 GiB instead of 2 for a 16384×8192 HDRI). Any float or uint channel widens the lot to
/// `Rgba32Float`, converted exactly as the general reader converts.
///
/// The general reader ([`decode_exr`]'s `rgba_channels` path) hands over one pixel at a time to a
/// closure on a single thread and builds an `f32` buffer that then had to be copied out as bytes;
/// on that 16K HDRI that was ~1.2 s of a ~1.5 s open. `None` sends the file there anyway: a first
/// layer without plain `R`, `G`, `B` channels (the names it requires), a subsampled channel, or
/// anything this reader fails on.
fn decode_exr_planes(bytes: &[u8], cx: &Cx) -> Option<DecodedImage> {
    use exr::prelude::*;

    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .on_progress(|p| cx.progress(p as f32))
        .from_buffered(Cursor::new(bytes))
        .ok()?;
    let layer = &image.layer_data;
    let list = &layer.channel_data.list;
    let find = |name: &str| list.iter().find(|c| c.name == *name);
    let mut channels = vec![find("R")?, find("G")?, find("B")?];
    channels.extend(find("A"));
    if channels.iter().any(|c| c.sampling != Vec2(1, 1)) {
        return None;
    }
    let n = layer.size.width() * layer.size.height();
    let has_alpha = channels.len() == 4;
    let halves: Option<Vec<&[f16]>> = channels
        .iter()
        .map(|c| match &c.sample_data {
            FlatSamples::F16(v) => Some(v.as_slice()),
            _ => None,
        })
        .collect();

    let (pixels, format) = if let Some(planes) = halves {
        if planes.iter().any(|p| p.len() < n) {
            return None;
        }
        const HALF_ONE: u16 = 0x3c00;
        let mut out = vec![0u8; n * 8];
        par_chunks_mut(&mut out, 8, |offset, part| {
            let first = offset / 8;
            for (i, px) in part.as_chunks_mut::<8>().0.iter_mut().enumerate() {
                let at = first + i;
                let a = planes.get(3).map_or(HALF_ONE, |p| p[at].to_bits());
                let rgba = [
                    planes[0][at].to_bits(),
                    planes[1][at].to_bits(),
                    planes[2][at].to_bits(),
                    a,
                ];
                for (c, v) in rgba.into_iter().enumerate() {
                    px[c * 2..c * 2 + 2].copy_from_slice(&v.to_ne_bytes());
                }
            }
        });
        (out, PixelFormat::Rgba16Float)
    } else {
        // The general reader's conversions: `f16::to_f32`, and a uint sample as its value.
        let sample = |s: &FlatSamples, at: usize| match s {
            FlatSamples::F16(v) => v[at].to_f32(),
            FlatSamples::F32(v) => v[at],
            FlatSamples::U32(v) => v[at] as f32,
        };
        if channels.iter().any(|c| c.sample_data.len() < n) {
            return None;
        }
        let mut out = vec![0u8; n * 16];
        par_chunks_mut(&mut out, 16, |offset, part| {
            let first = offset / 16;
            for (i, px) in part.as_chunks_mut::<16>().0.iter_mut().enumerate() {
                let at = first + i;
                let s = |c: usize| sample(&channels[c].sample_data, at);
                let a = if has_alpha { s(3) } else { 1.0 };
                for (c, v) in [s(0), s(1), s(2), a].into_iter().enumerate() {
                    px[c * 4..c * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                }
            }
        });
        (out, PixelFormat::Rgba32Float)
    };
    let dims = (layer.size.width(), layer.size.height());
    let channels = if has_alpha { 4 } else { 3 };
    Some(still(
        pixels,
        dims,
        format,
        channels,
        None,
        (Format::Exr, "OpenEXR"),
    ))
}

/// OpenEXR via the `exr` crate → 32-bit float RGBA (linear/HDR), for what
/// [`decode_exr_planes`] declines.
///
/// The headers are parsed on their own first, because the `rgba_channels` size closure below
/// cannot fail: it allocates 16 bytes per declared pixel and hands the buffer back by value, so a
/// crafted header would abort the process there ([`check_dims`]) with nothing able to intercept
/// it. Reading the metadata separately is the only place a dimension check *can* refuse.
fn decode_exr(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    use exr::prelude::*;

    struct Buf {
        width: usize,
        /// Whether the *file* declared an A channel. `rgba_channels` fills a missing one with
        /// an opaque lane, so the buffer is 4 wide either way and the pixels can't be asked.
        has_alpha: bool,
        pixels: Vec<[f32; 4]>,
    }

    let meta = exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    for header in &meta.headers {
        let size = header.layer_size;
        // 16 bytes/px: the closure's `[f32; 4]` buffer, and again for the `Vec<u8>` built from it.
        cx.check_dims(size.width(), size.height(), 16, "OpenEXR")?;
    }

    if let Some(img) = decode_exr_planes(bytes, cx) {
        return Ok(img);
    }

    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(
            |size, channels: &RgbaChannels| Buf {
                width: size.width(),
                has_alpha: channels.3.is_some(),
                pixels: vec![[0.0f32; 4]; size.width() * size.height()],
            },
            |buf: &mut Buf, pos, (r, g, b, a): (f32, f32, f32, f32)| {
                // The position comes from the decoder, not from us: bounds-check it rather
                // than index, so a crafted file cannot panic a decode worker.
                let i = pos.y() * buf.width + pos.x();
                if let Some(px) = buf.pixels.get_mut(i) {
                    *px = [r, g, b, a];
                }
            },
        )
        .first_valid_layer()
        .all_attributes()
        .on_progress(|p| cx.progress(p as f32))
        .from_buffered(Cursor::new(bytes))
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;

    let size = image.layer_data.size;
    let buf = image.layer_data.channel_data.pixels;
    let pixels = to_ne_bytes(&buf.pixels);
    Ok(DecodedImage {
        pixels,
        width: size.width() as u32,
        height: size.height() as u32,
        format: PixelFormat::Rgba32Float,
        // Report what the file actually carries. An RGB-only EXR gets an opaque alpha lane
        // synthesized by the reader, and claiming 4 channels for it offered the viewer's
        // alpha UI over a channel the file never had — the same untruth a 24-bit TGA must not
        // tell (see `decode_image`).
        source: SourceInfo::new(
            Format::Exr,
            "OpenEXR",
            if buf.has_alpha { 4 } else { 3 },
            32,
        ),
        icc: None,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    })
}

/// HEIC/HEIF/AVIF via libheif (libde265 for HEVC, dav1d for AV1), through heif-sys's ABI v2
/// (FD16). The C++ wrapper catches its own exceptions, and libheif reports malformed input as an
/// error code.
///
/// - **Limits:** the setup's. The wrapper checks an item's signalled size against them before
///   libheif decodes it, and sets libheif's own security limits from them.
/// - **Threads:** a lease on the budget for the whole call; the wrapper gives a grid's tiles their
///   own threads and a single image the codec's (FD8).
/// - **Progress and cancel** go to libheif, which reports per grid tile.
/// - **Sequence-only files** (no primary image) show their first frame (FD16).
///
/// 8-bit sources come back as `Rgba8Unorm`; 10/12-bit as `Rgba16Unorm` scaled to full range. The
/// nclx box, when there is one, describes the source's transfer and primaries. libheif applies
/// the YCbCr matrix but converts neither (measured in step 2b): a Display P3 file comes back in
/// P3 for the `icc` pass to convert, and a PQ / HLG one as the encoded signal, which
/// [`hdr::convert`] decodes to light for the setup's [`Hdr`] (FD17, step 6b).
#[cfg(feature = "heif")]
fn decode_heif(bytes: &[u8], format: Format, cx: &Cx) -> Result<DecodedImage, DecodeError> {
    use heif_sys::HeifError;
    const MAX_ICC_LEN: usize = 16 << 20;
    // The boxes libheif has no API for (gain maps, FD17), read once here.
    let found = heif_meta::find_meta(bytes);
    let meta = found.and_then(|(m, _)| heif_meta::Meta::parse(m).ok());
    // A `tmap` primary, which libheif can't open: point `pitm` at its SDR base, in a copy.
    let patched;
    let bytes = match (&meta, found) {
        (Some(m), Some((_, meta_at))) => match (m.tmap_primary_base(), m.pitm_field()) {
            (Some(base), Some((field, width))) if width == 4 || base <= u16::MAX as u32 => {
                let mut copy = bytes.to_vec();
                let at = meta_at + field;
                if width == 2 {
                    copy[at..at + 2].copy_from_slice(&(base as u16).to_be_bytes());
                } else {
                    copy[at..at + 4].copy_from_slice(&base.to_be_bytes());
                }
                patched = copy;
                &patched[..]
            }
            _ => bytes,
        },
        _ => bytes,
    };
    let base = meta
        .as_ref()
        .and_then(|m| m.tmap_primary_base().or(m.primary));
    let gain_map = meta
        .as_ref()
        .zip(base)
        .and_then(|(m, b)| m.gain_map(b).map(|g| (b, g)));
    // With `frames: All`, an image sequence plays rather than its still (FD18). One that can't be
    // read leaves the still, when the file has one; a cancel or a refusal on size stands.
    if cx.opts.frames == Frames::All
        && let Some(moov) = heif_meta::boxes(bytes)
            .find(|b| b.0 == *b"moov")
            .map(|b| b.1)
    {
        match decode_heif_sequence(bytes, moov, format, cx) {
            Ok(img) => return Ok(img),
            Err(e @ (DecodeError::Cancelled | DecodeError::TooLarge(_))) => return Err(e),
            Err(e) if meta.as_ref().and_then(|m| m.primary).is_none() => return Err(e),
            Err(_) => {}
        }
    }
    // Under `Linear` a gain-map photo renders as HDR. A gain map that can't be read leaves the
    // SDR base, as `Sdr` shows it; a cancel or a refusal on size stands.
    if let (Hdr::Linear, Some(m), Some((b, g))) = (cx.opts.hdr, &meta, gain_map) {
        match gainmap::render(bytes, m, b, g, format, cx) {
            Ok(img) => return Ok(img),
            Err(e @ (DecodeError::Cancelled | DecodeError::TooLarge(_))) => return Err(e),
            Err(_) => {}
        }
    }
    let lim = cx.limits();
    let limits = heif_sys::Limits {
        max_dim: lim.max_dim,
        max_output_bytes: lim.max_bytes,
        max_total_memory: lim.max_bytes.saturating_mul(2),
        max_tiles: 0,
        max_sequence_frames: MAX_ANIMATION_FRAMES as u32,
        max_color_profile_size: MAX_ICC_LEN as u32,
    };
    let lease = budget::lease(budget::cores());
    let progress = |p: f32| cx.progress(p);
    let cancel = || cx.cancelled();
    let ctl = heif_sys::Ctl {
        library_threads: lease.threads(),
        progress: Some(&progress),
        cancel: Some(&cancel),
        ..heif_sys::Ctl::default()
    };
    type Decoded = (heif_sys::Image, Option<Vec<u8>>, Option<heif_sys::Nclx>);
    let decode = || -> Result<Decoded, HeifError> {
        let mut file = heif_sys::HeifFile::open(bytes, &limits)?;
        match file.primary_id() {
            Some(id) => {
                let info = file.item_info(id)?;
                let icc = file.item_icc(&info, MAX_ICC_LEN);
                Ok((file.decode_item(id, &ctl)?, icc, info.nclx))
            }
            None => {
                // No primary image: the first frame of the first visual track stands in.
                let mut track = file.first_track()?;
                let frame = track.next_frame(&ctl)?.ok_or(HeifError::NoImage)?;
                Ok((frame, None, None))
            }
        }
    };
    let (img, icc, nclx) = decode().map_err(|e| match e {
        HeifError::TooLarge => DecodeError::TooLarge(e.to_string()),
        HeifError::Cancelled => DecodeError::Cancelled,
        HeifError::OutOfMemory { .. } => DecodeError::Other(e.to_string()),
        HeifError::Libheif(_) | HeifError::BadImage | HeifError::NoImage | HeifError::NotFound => {
            DecodeError::Malformed(e.to_string())
        }
        HeifError::Exception | HeifError::InitFailed | HeifError::Ffi(_) => {
            DecodeError::Ffi(e.to_string())
        }
    })?;
    drop(lease);

    let (pixel_format, bit_depth) = if img.is_16bit {
        (PixelFormat::Rgba16Unorm, img.bit_depth)
    } else {
        (PixelFormat::Rgba8Unorm, 8)
    };
    // Report the source channel count for the status bar (4 with alpha, else 3).
    let mut source = SourceInfo::new(
        format,
        format.name(),
        if img.has_alpha { 4 } else { 3 },
        bit_depth,
    );
    if let Some(nclx) = nclx {
        (source.transfer, source.primaries, source.hdr) =
            source::from_nclx(nclx.transfer_characteristics, nclx.colour_primaries);
    }
    if gain_map.is_some() && source.hdr == HdrKind::None {
        source.hdr = HdrKind::GainMap;
    }
    let mut img = DecodedImage {
        pixels: img.pixels,
        width: img.width,
        height: img.height,
        format: pixel_format,
        source,
        icc,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    };
    hdr::convert(&mut img, cx.opts.hdr, cx)?;
    Ok(img)
}

/// An image sequence (`avis`, or HEIF's `msf1`), every frame of its first visual track as
/// [`Animation`] frames (FD18). The track's media timeline is played once, its edit list ignored,
/// so an infinitely looping file ends; the repetitions become [`Animation::loop_count`]. Frames
/// arrive without a count, so GIF's caps (1 GiB of frames, 10,000 of them) are applied as they
/// arrive, and a cut is [`Animation::truncated`]. Progress is per frame, against the track's
/// sample count; cancel is polled between frames and by libheif within one. A PQ / HLG track is
/// converted frame by frame, as a still is (FD17).
#[cfg(feature = "heif")]
fn decode_heif_sequence(
    bytes: &[u8],
    moov: &[u8],
    format: Format,
    cx: &Cx,
) -> Result<DecodedImage, DecodeError> {
    use heif_sys::HeifError;
    let (count, nclx) = heif_meta::track_facts(moov);
    let lim = cx.limits();
    let limits = heif_sys::Limits {
        max_dim: lim.max_dim,
        max_output_bytes: lim.max_bytes,
        max_total_memory: lim.max_bytes.saturating_mul(2),
        max_tiles: 0,
        max_sequence_frames: MAX_ANIMATION_FRAMES as u32,
        max_color_profile_size: 0,
    };
    let lease = budget::lease(budget::cores());
    let cancel = || cx.cancelled();
    let ctl = heif_sys::Ctl {
        library_threads: lease.threads(),
        cancel: Some(&cancel),
        ..heif_sys::Ctl::default()
    };
    let err = |e: HeifError| match e {
        HeifError::TooLarge => DecodeError::TooLarge(e.to_string()),
        HeifError::Cancelled => DecodeError::Cancelled,
        HeifError::OutOfMemory { .. } => DecodeError::Other(e.to_string()),
        e => DecodeError::Malformed(e.to_string()),
    };
    let mut file = heif_sys::HeifFile::open(bytes, &limits).map_err(err)?;
    let mut track = file.first_track().map_err(err)?;
    let info = track.info().map_err(err)?;
    let first = track
        .next_frame(&ctl)
        .map_err(err)?
        .ok_or_else(|| DecodeError::Malformed("HEIF: the sequence has no frames".into()))?;
    let bpp = if first.is_16bit { 8 } else { 4 };
    cx.check_dims(first.width as usize, first.height as usize, bpp, "HEIF")?;
    let frame_bytes = (first.width as u64 * first.height as u64 * bpp as u64).max(1);
    let sequence_bytes = MAX_ANIMATION_BYTES.min(lim.max_bytes);
    let max_frames = ((sequence_bytes / frame_bytes) as usize).clamp(1, MAX_ANIMATION_FRAMES);
    let total = count.filter(|&n| n > 0);
    let report = |done: usize| {
        cx.progress(match total {
            Some(n) => (done as f32 / n as f32).min(1.0),
            // No count: rise towards 1 without reaching it.
            None => 1.0 - 1.0 / (done as f32 + 1.0),
        })
    };
    report(1);
    let mut truncated = false;
    let (w, h, is_16bit, bit_depth) = (first.width, first.height, first.is_16bit, first.bit_depth);
    let delay = |d: u32| {
        let ms = (d as u64 * 1000 + info.timescale as u64 / 2)
            .checked_div(info.timescale as u64)
            .unwrap_or(0);
        if ms == 0 {
            100
        } else {
            ms.min(u32::MAX as u64) as u32
        }
    };
    let mut frames = vec![AnimationFrame {
        delay_ms: delay(first.duration),
        pixels: first.pixels,
    }];
    while frames.len() < max_frames {
        cx.check_cancel()?;
        let Some(frame) = track.next_frame(&ctl).map_err(err)? else {
            break;
        };
        // A frame of another size or depth can't share the canvas: the sequence stops there.
        if (frame.width, frame.height, frame.is_16bit) != (w, h, is_16bit) {
            truncated = true;
            break;
        }
        frames.push(AnimationFrame {
            delay_ms: delay(frame.duration),
            pixels: frame.pixels,
        });
        report(frames.len());
    }
    // The cap cut it short when the track has samples left. Without a count, a frame that would
    // have been next is assumed.
    if frames.len() >= max_frames {
        truncated |= total.is_none_or(|n| n as usize > frames.len());
    }
    drop(track);
    drop(lease);

    let mut source = SourceInfo::new(
        format,
        format.name(),
        if info.has_alpha { 4 } else { 3 },
        bit_depth,
    );
    if let Some((primaries, transfer)) = nclx {
        (source.transfer, source.primaries, source.hdr) = source::from_nclx(transfer, primaries);
    }
    let pixels = frames[0].pixels.clone();
    let animation = (frames.len() > 1).then(|| Animation {
        frames,
        truncated,
        loop_count: (info.repetitions != heif_sys::REPETITIONS_INFINITE)
            .then_some(info.repetitions),
    });
    let mut img = DecodedImage {
        pixels,
        width: w,
        height: h,
        format: if is_16bit {
            PixelFormat::Rgba16Unorm
        } else {
            PixelFormat::Rgba8Unorm
        },
        source,
        icc: None,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation,
    };
    hdr::convert(&mut img, cx.opts.hdr, cx)?;
    Ok(img)
}

/// Stand-in for [`decode_heif`] when the crate is built without the vendored libheif stack —
/// see [`decode_psd`]'s counterpart for why this errors rather than rerouting.
#[cfg(not(feature = "heif"))]
fn decode_heif(_bytes: &[u8], format: Format, _cx: &Cx) -> Result<DecodedImage, DecodeError> {
    Err(DecodeError::Unsupported(format))
}

/// Camera raw via embedded-preview extraction ([`raw`]). The raw container is not decoded;
/// instead we locate the largest embedded JPEG preview the camera wrote and decode *that*
/// through the normal zune JPEG path, so ICC handling, downscale, and the rest apply
/// unchanged. The pixels are the camera's own rendering (white-balanced, 8-bit) — the right
/// trade for a fast viewer, and we apply the file's EXIF orientation so portrait shots are
/// upright. Developing the sensor mosaic (demosaic/color matrices) is out of scope (§6).
///
/// Pure-Rust and bounds-checked end to end, but the worker pool still wraps the whole decode
/// in `catch_unwind` as a backstop, same as every other backend.
#[cfg(feature = "raw")]
fn decode_raw(bytes: &[u8], label: &'static str, cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let preview = raw::find_preview(bytes)
        .ok_or_else(|| DecodeError::Malformed("raw file has no embedded JPEG preview".into()))?;
    // The preview is a JPEG; reuse the zune path (ICC + bit-depth + naming) then re-label it
    // as the raw family and, when the setup honours EXIF orientation, orient it.
    let mut img = decode_zune(preview.jpeg, zune_image::codecs::ImageFormat::JPEG, cx)?;
    if cx.opts.honor_exif {
        exif::apply(&mut img, preview.orientation);
    }
    img.source.format = Format::CameraRaw;
    img.source.name = label;
    Ok(img)
}

/// Stand-in for [`decode_raw`] in a build without `raw`.
#[cfg(not(feature = "raw"))]
fn decode_raw(_bytes: &[u8], _label: &'static str, _cx: &Cx) -> Result<DecodedImage, DecodeError> {
    Err(DecodeError::Unsupported(Format::CameraRaw))
}

/// Radiance HDR via the `image` crate → 32-bit float RGBA (linear/HDR).
///
/// Deliberately *off* the zune hot path, for two reasons measured on real files:
/// - **Correctness:** zune-hdr (≤ 0.5.2, and upstream `dev` as of 2026-07) computes the RGBE
///   scale `2^(E-128)` with a shift masked `& 31`, so any pixel with `|E-128| >= 32` wraps —
///   near-black values (E ≤ 96) come back exactly 2^32 too bright, rendering as bright
///   blue/green patches in dark regions.
/// - **Speed:** the `image` crate decodes the same files ~2× faster than zune-hdr, so this
///   routing also serves the time-to-first-pixel metric.
///
/// Non-strict mode accepts the `#?RGBE` signature variant and old signature-less `.pic`
/// files that the strict `#?RADIANCE` check would reject. Radiance carries no ICC profile;
/// the data is linear RGB, so the HDR exposure/tonemap path applies downstream.
fn decode_hdr(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    use image::ImageDecoder;

    // RGBE is read front to back, scanline by scanline, so how far into the file the decoder has
    // read is how far through the image it is.
    let progress = |p: f32| cx.progress(p);
    let reader = ProgressReader::new(bytes, &progress);
    let decoder = image::codecs::hdr::HdrDecoder::with_strictness(reader, false)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    // Constructed directly (not via `ImageReader`), so the reader's default memory limits don't
    // apply — the header guard is ours to make, exactly as in `decode_png`. `into_rgba32f_bytes` below
    // allocates 16 bytes per declared pixel.
    let (w, h) = decoder.dimensions();
    cx.check_dims(w as usize, h as usize, 16, "Radiance HDR")?;

    let dynimg = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let (width, height) = (dynimg.width(), dynimg.height());

    let pixels = into_rgba32f_bytes(dynimg);

    Ok(DecodedImage {
        pixels,
        width,
        height,
        format: PixelFormat::Rgba32Float,
        // RGBE is always RGB; the alpha lane is added by normalization.
        source: SourceInfo::new(Format::RadianceHdr, "Radiance HDR", 3, 32),
        icc: None,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    })
}

/// An in-memory reader that reports the fraction of `bytes` read so far, for a backend that
/// consumes its input front to back.
///
/// The RGBE reader asks for a byte at a time, ~200M times for a 16k panorama, so anything added to
/// a read is paid that often — a wrapper that so much as compared a position per read measured
/// 5–8% slower than the bare `Cursor`. Here a read is served by the standard `&[u8]` reader over a
/// *window* that ends where the next report is due, so the hot path is exactly the slice reader;
/// only running off the window's end (every 1/256th of the input) reports and moves it on.
struct ProgressReader<'a> {
    bytes: &'a [u8],
    /// The unread rest of the current window.
    window: &'a [u8],
    /// Where `window` ends in `bytes`.
    end: usize,
    progress: &'a dyn Fn(f32),
}

impl<'a> ProgressReader<'a> {
    fn new(bytes: &'a [u8], progress: &'a dyn Fn(f32)) -> Self {
        Self {
            bytes,
            window: &[],
            end: 0,
            progress,
        }
    }

    /// Report the position, and open a window from it that holds at least `want` bytes (as far as
    /// the input goes) and reaches the next report.
    #[cold]
    #[inline(never)]
    fn next_window(&mut self, want: usize) {
        let len = self.bytes.len();
        let pos = self.end - self.window.len();
        (self.progress)(pos as f32 / len.max(1) as f32);
        self.end = (pos + want.max(len / 256).max(1)).min(len);
        self.window = &self.bytes[pos..self.end];
    }
}

impl std::io::Read for ProgressReader<'_> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.window.is_empty() {
            self.next_window(buf.len());
        }
        self.window.read(buf)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        if buf.len() > self.window.len() {
            self.next_window(buf.len());
        }
        self.window.read_exact(buf)
    }
}

/// `DynamicImage::into_rgba8`, as a raw buffer — minus the `image` crate's slow paths.
///
/// Since 0.25 the crate's RGBA conversions go through a colour-space-aware cast that has direct
/// paths only for same-layout and RGB → RGBA. Grey and grey+alpha fall back to expanding every
/// pixel through `f32` and back, which made an 8192² greyscale TGA spend ~420 ms converting —
/// nearly 10× what the three-times-larger RGB file took — and even the direct RGB path runs on one
/// thread. Every layout here is a plain widening (the sample copied to R, G and B, opaque alpha),
/// so [`rgba_bytes`] does it; an RGBA source is handed over as is, and anything else is left to
/// the crate.
fn into_rgba8(img: image::DynamicImage) -> Vec<u8> {
    use image::DynamicImage;
    match img {
        DynamicImage::ImageLuma8(b) => rgba_bytes(b.as_raw(), 1, u8::MAX),
        DynamicImage::ImageLumaA8(b) => rgba_bytes(b.as_raw(), 2, u8::MAX),
        DynamicImage::ImageRgb8(b) => rgba_bytes(b.as_raw(), 3, u8::MAX),
        DynamicImage::ImageRgba8(b) => b.into_raw(),
        other => other.into_rgba8().into_raw(),
    }
}

/// [`into_rgba8`] for 16-bit sources, straight to the native-endian bytes `Rgba16Unorm` carries
/// (no intermediate `Vec<u16>` to copy out of).
fn into_rgba16_bytes(img: image::DynamicImage) -> Vec<u8> {
    use image::DynamicImage;
    match img {
        DynamicImage::ImageLuma16(b) => rgba_bytes(b.as_raw(), 1, u16::MAX),
        DynamicImage::ImageLumaA16(b) => rgba_bytes(b.as_raw(), 2, u16::MAX),
        DynamicImage::ImageRgb16(b) => rgba_bytes(b.as_raw(), 3, u16::MAX),
        DynamicImage::ImageRgba16(b) => rgba_bytes(b.as_raw(), 4, u16::MAX),
        other => to_ne_bytes(other.into_rgba16().as_raw()),
    }
}

/// [`into_rgba8`] for float sources, straight to the native-endian bytes `Rgba32Float` carries.
fn into_rgba32f_bytes(img: image::DynamicImage) -> Vec<u8> {
    use image::DynamicImage;
    match img {
        DynamicImage::ImageRgb32F(b) => rgba_bytes(b.as_raw(), 3, 1.0f32),
        DynamicImage::ImageRgba32F(b) => rgba_bytes(b.as_raw(), 4, 1.0f32),
        other => to_ne_bytes(other.into_rgba32f().as_raw()),
    }
}

/// Widen interleaved samples — grey (`channels == 1`), grey+alpha (2), RGB (3) or RGBA (4) — to
/// RGBA, as the native-endian bytes every [`PixelFormat`] is carried in. Grey is copied to R, G
/// and B; a source without alpha gets `opaque`.
///
/// This is every backend's last pass over the pixels, so it is written to cost one read and one
/// write of the buffer: split across threads ([`par_chunks_mut`]), each part filled as fixed-size
/// pixel arrays (the shape LLVM vectorizes), and written straight into the byte buffer that is
/// returned rather than into a typed one that would then be copied out.
pub(crate) fn rgba_bytes<T: bytemuck::Pod + Sync>(
    src: &[T],
    channels: usize,
    opaque: T,
) -> Vec<u8> {
    debug_assert!(matches!(channels, 1..=4));
    let size = std::mem::size_of::<T>();
    let pixels = src.len() / channels;
    let mut out = vec![0u8; pixels * 4 * size];
    par_chunks_mut(&mut out, 4 * size, |offset, part| {
        let first = offset / (4 * size);
        let n = part.len() / (4 * size);
        let src = &src[first * channels..(first + n) * channels];
        match bytemuck::try_cast_slice_mut::<u8, T>(part) {
            Ok(typed) => widen_to_rgba(src, channels, opaque, typed),
            // A byte buffer is only guaranteed byte alignment. Every allocator fire runs on
            // hands back more for a buffer this size, so this is the path never taken — but
            // it must still be correct: widen into an aligned scratch and copy its bytes.
            Err(_) => {
                let mut scratch = vec![opaque; n * 4];
                widen_to_rgba(src, channels, opaque, &mut scratch);
                part.copy_from_slice(bytemuck::cast_slice(&scratch));
            }
        }
    });
    out
}

/// The per-part body of [`rgba_bytes`]: `src` holds exactly the pixels `out` (RGBA) has room for.
fn widen_to_rgba<T: Copy>(src: &[T], channels: usize, opaque: T, out: &mut [T]) {
    let px = out.as_chunks_mut::<4>().0;
    match channels {
        1 => {
            for (o, &g) in px.iter_mut().zip(src) {
                *o = [g, g, g, opaque];
            }
        }
        2 => {
            for (o, &[g, a]) in px.iter_mut().zip(src.as_chunks::<2>().0) {
                *o = [g, g, g, a];
            }
        }
        3 => {
            for (o, &[r, g, b]) in px.iter_mut().zip(src.as_chunks::<3>().0) {
                *o = [r, g, b, opaque];
            }
        }
        _ => out.copy_from_slice(src),
    }
}

/// PNG via the `image` crate → RGBA8, or RGBA16 for 16-bit sources (precision preserved
/// for the inspector / HDR pipeline). Extracts the embedded ICC profile.
///
/// Deliberately *off* the zune hot path: on large real-world PNGs the `image` crate's
/// `png`+`fdeflate` stack decodes ~1.8× faster than zune-png end-to-end (measured 2026-07
/// on 8192×4096 game textures: ~190 ms vs ~340 ms including RGBA normalization), and the
/// gap is in the core decode, not wrapper overhead. Constructed directly rather than via
/// `image::ImageReader` so the reader's default memory limits don't reject large-but-real
/// images (a 216-MP scan trips them); the [`check_dims`] header guard here and the setup's fit
/// are the actual bomb/RAM guards, matching the zune path.
fn decode_png(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    use image::{DynamicImage, ImageDecoder};

    let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    // 16-bit sources are kept at 16 bits (8 bytes/px RGBA); everything else normalizes to RGBA8.
    // Mirrors the `is_16bit` split below — the buffer this sizes is the one it allocates.
    let out_bpp = match decoder.color_type() {
        image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => 8,
        _ => 4,
    };
    cx.check_dims(width as usize, height as usize, out_bpp, "PNG")?;
    // Source channel count (status bar / alpha-aware UI) and ICC must be read before
    // `from_decoder` consumes the decoder. Palette sources already report their expanded
    // RGB/RGBA color type, matching what zune reported.
    let src_channels = decoder.color_type().channel_count();
    let icc = decoder.icc_profile().ok().flatten();

    let dynimg =
        DynamicImage::from_decoder(decoder).map_err(|e| DecodeError::Malformed(e.to_string()))?;

    let is_16bit = matches!(
        dynimg,
        DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
            | DynamicImage::ImageRgb16(_)
            | DynamicImage::ImageRgba16(_)
    );
    // `image` reports a 1/2/4-bit or palette PNG as the 8-bit colour it expands to; the file's own
    // depth comes from its header.
    let (bit_depth, palette) = match png_ihdr(bytes) {
        Some((depth, colour_type)) => (depth, colour_type == 3),
        None => (if is_16bit { 16 } else { 8 }, false),
    };
    let (pixels, format) = if is_16bit {
        // The CPU shader reads Rgba16Unorm back as native-endian u16.
        (into_rgba16_bytes(dynimg), PixelFormat::Rgba16Unorm)
    } else {
        (into_rgba8(dynimg), PixelFormat::Rgba8Unorm)
    };

    Ok(DecodedImage {
        pixels,
        width,
        height,
        format,
        source: SourceInfo {
            palette,
            ..SourceInfo::new(Format::Png, "PNG", src_channels, bit_depth)
        },
        icc,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    })
}

/// The bit depth and colour type a PNG's header declares. IHDR is always the first chunk — after
/// the 8-byte signature, its length and type, then width and height — and `PngDecoder::new` has
/// already validated it by the time this is read.
fn png_ihdr(bytes: &[u8]) -> Option<(u8, u8)> {
    let ihdr = bytes.get(12..26)?;
    (&ihdr[..4] == b"IHDR").then_some((ihdr[12], ihdr[13]))
}

/// Refuse an oversized zune source *before* [`decode_zune`]'s `Image::read` allocates from its
/// header.
///
/// The zune path decodes in one shot — `Image::read` parses the header and allocates the pixels
/// without ever handing us the dimensions in between — so the header is parsed a *second* time
/// here, on a throwaway decoder, purely to have somewhere to say no. zune's
/// `set_max_width`/`set_max_height` bound each axis independently and nothing bounds the *product*,
/// so a 65535×65535 JPEG sits under both caps while asking for ~17 GiB.
///
/// Decode speed is this project's primary metric, so the second parse was measured rather than
/// assumed: **13.6 µs against a 129 ms decode** on a 4928×3264 JPEG — 0.011%, because it reads
/// marker bytes and no pixels. It scales with header size, not image size (7.5 µs on a 4 MP file),
/// so it does not grow with the images it protects.
///
/// Every format routed here implements `read_headers` (`zune_read_headers_is_not_vacuous` pins
/// that); a decoder that did not would fall back to zune's per-axis caps alone.
fn check_zune_dims(
    bytes: &[u8],
    fmt: zune_image::codecs::ImageFormat,
    opts: zune_core::options::DecoderOptions,
    cx: &Cx,
) -> Result<(), DecodeError> {
    use zune_core::bit_depth::BitDepth;
    use zune_core::bytestream::ZCursor;

    let mut decoder = fmt
        .decoder_with_options(ZCursor::new(bytes), opts)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let headers = decoder
        .read_headers()
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let Some(md) = headers else {
        return Ok(());
    };

    let (width, height) = md.dimensions();
    // `decode_zune` normalizes every colorspace to RGBA at the source's bit depth, so *that* is the
    // buffer being sized — not the source's own channel count.
    let bpp = match md.depth() {
        BitDepth::Sixteen => 8,
        BitDepth::Float32 => 16,
        _ => 4,
    };
    cx.check_dims(width, height, bpp, zune_format_name(fmt))
}

/// The speed-first options every zune decoder runs with: platform intrinsics and unsafe fast
/// paths on, and the dimension guard set to the setup's own rather than zune's 16384 default, so
/// large sources decode (the fit pass shrinks them afterwards).
fn zune_options(cx: &Cx) -> zune_core::options::DecoderOptions {
    let max = cx.limits().max_dim as usize;
    zune_core::options::DecoderOptions::new_fast()
        .set_max_width(max)
        .set_max_height(max)
}

/// The hot path: zune for JPEG/BMP/QOI/PPM/WebP/farbfeld/JPEG-XL.
///
/// Each format's own decoder is called directly and its interleaved output widened to RGBA in one
/// pass ([`rgba_bytes`]). `zune_image::Image::read`, which this used to go through for all of them,
/// splits the decoded pixels into one plane per channel, allocates and fills an alpha plane, and
/// interleaves the lot back together: three extra passes over the image, ~70 ms of an 8192²
/// JPEG's ~400. It remains the fallback for what the direct paths decline ([`decode_zune_image`]).
fn decode_zune(
    bytes: &[u8],
    fmt: zune_image::codecs::ImageFormat,
    cx: &Cx,
) -> Result<DecodedImage, DecodeError> {
    use zune_image::codecs::ImageFormat as Z;
    let direct = match fmt {
        Z::JPEG => Some(decode_jpeg(bytes, cx)?),
        Z::BMP => decode_bmp(bytes, cx)?,
        Z::QOI => Some(decode_qoi(bytes, cx)?),
        Z::PPM => decode_ppm(bytes, cx)?,
        Z::Farbfeld => Some(decode_farbfeld(bytes, cx)?),
        Z::WEBP => decode_webp(bytes, cx)?,
        Z::JPEG_XL => decode_jxl(bytes, cx)?,
        _ => None,
    };
    match direct {
        Some(img) => Ok(img),
        None => decode_zune_image(bytes, fmt, cx),
    }
}

fn malformed(e: impl std::fmt::Debug) -> DecodeError {
    DecodeError::Malformed(format!("{e:?}"))
}

/// The fields every still, single-level image fills the same way; the source's bit depth is the
/// buffer's.
fn still(
    pixels: Vec<u8>,
    (width, height): (usize, usize),
    format: PixelFormat,
    channels: u8,
    icc: Option<Vec<u8>>,
    (source_format, name): (Format, &'static str),
) -> DecodedImage {
    DecodedImage {
        pixels,
        width: width as u32,
        height: height as u32,
        format,
        source: SourceInfo::new(source_format, name, channels, format.bits()),
        icc,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    }
}

/// An 8-bit interleaved zune buffer in `cs`, widened to RGBA, with the source's channel count.
/// `None` for a layout this does not widen (CMYK, BGR, …), which sends the file to
/// [`decode_zune_image`].
fn zune_rgba8(cs: zune_core::colorspace::ColorSpace, px: Vec<u8>) -> Option<(Vec<u8>, u8)> {
    use zune_core::colorspace::ColorSpace;
    let channels = match cs {
        ColorSpace::Luma => 1,
        ColorSpace::LumaA => 2,
        ColorSpace::RGB => 3,
        ColorSpace::RGBA => return Some((px, 4)),
        _ => return None,
    };
    Some((rgba_bytes(&px, channels, u8::MAX), channels as u8))
}

/// JPEG through zune-jpeg, decoded straight into RGBA. The header is read once, here, and the
/// [`check_dims`] guard applied to it before anything is allocated.
///
/// CMYK/YCCK are decoded to RGB and widened rather than asked for as RGBA: zune-jpeg's RGBA output
/// converts ink through a different path than its RGB one, and the RGB one is what fire has
/// always shown.
fn decode_jpeg(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;

    // The output colorspace has to be chosen before the header is read — zune-jpeg sets up its
    // per-component output state from it there — so CMYK, which only the header reveals, costs a
    // second (microseconds-long) header parse.
    let open = |out: ColorSpace| -> Result<_, DecodeError> {
        let opts = zune_options(cx).jpeg_set_out_colorspace(out);
        let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
        d.decode_headers().map_err(malformed)?;
        Ok(d)
    };
    let mut d = open(ColorSpace::RGBA)?;
    let info = d
        .info()
        .ok_or_else(|| malformed("JPEG has no frame header"))?;
    let dims = (info.width as usize, info.height as usize);
    cx.check_dims(dims.0, dims.1, 4, "JPEG")?;
    let input = d.input_colorspace().unwrap_or(ColorSpace::RGB);
    let ink = matches!(input, ColorSpace::CMYK | ColorSpace::YCCK);
    if ink {
        d = open(ColorSpace::RGB)?;
    }
    let px = d.decode().map_err(malformed)?;
    let pixels = if ink { rgba_bytes(&px, 3, u8::MAX) } else { px };
    // The channels the *file* carries, for the status bar: a JPEG has no alpha, and its grey
    // variant is one channel even though it is shown as RGBA.
    let channels = if input.num_components() == 1 { 1 } else { 3 };
    let icc = d.icc_profile();
    Ok(still(
        pixels,
        dims,
        PixelFormat::Rgba8Unorm,
        channels,
        icc,
        (Format::Jpeg, "JPEG"),
    ))
}

/// BMP. 24-bit goes through zune-bmp (the faster of the two decoders for it); every other depth
/// through the `image` crate, whose 32-bit and palette paths measured 3× and 1.8× faster than
/// zune-bmp's on an 8192² file (156 vs 502 ms, 264 vs 476 ms) for identical pixels. `None`
/// (→ the generic zune path) only for a 24-bit layout zune hands back as something unexpected.
fn decode_bmp(bytes: &[u8], cx: &Cx) -> Result<Option<DecodedImage>, DecodeError> {
    use zune_core::bytestream::ZCursor;

    // BITMAPINFOHEADER and every later version: header size at 14, bits per pixel at 28. The
    // 12-byte OS/2 header keeps its depth elsewhere, and goes to the `image` crate, which reads it.
    let le16 = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let le32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if !(le32(14).is_some_and(|h| h >= 40) && le16(28) == Some(24)) {
        return decode_image(bytes, Some("bmp"), cx).map(Some);
    }
    let mut d = zune_bmp::BmpDecoder::new_with_options(ZCursor::new(bytes), zune_options(cx));
    d.decode_headers().map_err(malformed)?;
    let dims = d
        .dimensions()
        .ok_or_else(|| malformed("BMP has no dimensions"))?;
    cx.check_dims(dims.0, dims.1, 4, "BMP")?;
    let cs = d
        .colorspace()
        .ok_or_else(|| malformed("BMP has no colorspace"))?;
    let px = d.decode().map_err(malformed)?;
    Ok(zune_rgba8(cs, px).map(|(pixels, ch)| {
        still(
            pixels,
            dims,
            PixelFormat::Rgba8Unorm,
            ch,
            None,
            (Format::Bmp, "BMP"),
        )
    }))
}

/// QOI through the `qoi` crate, decoded straight to RGBA — an RGB file gets its opaque alpha in
/// the same pass. QOI is a strictly sequential format, so the decoder's inner loop is the whole
/// cost: zune-qoi spent ~530 ms on an 8192² RGBA file that this does in a fraction of that.
fn decode_qoi(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let d = qoi::Decoder::new(bytes).map_err(malformed)?;
    let header = *d.header();
    let dims = (header.width as usize, header.height as usize);
    cx.check_dims(dims.0, dims.1, 4, "QOI")?;
    let pixels = d
        .with_channels(qoi::Channels::Rgba)
        .decode_to_vec()
        .map_err(malformed)?;
    let channels = header.channels.as_u8();
    Ok(still(
        pixels,
        dims,
        PixelFormat::Rgba8Unorm,
        channels,
        None,
        (Format::Qoi, "QOI"),
    ))
}

/// PPM/PGM/PBM/PAM, and the float PFM, at the file's own depth.
fn decode_ppm(bytes: &[u8], cx: &Cx) -> Result<Option<DecodedImage>, DecodeError> {
    use zune_core::bit_depth::BitDepth;
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::result::DecodingResult;

    let mut d = zune_ppm::PPMDecoder::new_with_options(ZCursor::new(bytes), zune_options(cx));
    d.decode_headers().map_err(malformed)?;
    let dims = d
        .dimensions()
        .ok_or_else(|| malformed("PPM has no dimensions"))?;
    let cs = d
        .colorspace()
        .ok_or_else(|| malformed("PPM has no colorspace"))?;
    let channels = match cs {
        ColorSpace::Luma => 1,
        ColorSpace::LumaA => 2,
        ColorSpace::RGB => 3,
        ColorSpace::RGBA => 4,
        _ => return Ok(None),
    };
    let sample = match d.bit_depth() {
        Some(BitDepth::Sixteen) => 2,
        Some(BitDepth::Float32) => 4,
        _ => 1,
    };
    cx.check_dims(dims.0, dims.1, 4 * sample, "PPM")?;
    let (pixels, format) = match d.decode().map_err(malformed)? {
        DecodingResult::U8(v) => (rgba_bytes(&v, channels, u8::MAX), PixelFormat::Rgba8Unorm),
        DecodingResult::U16(v) => (rgba_bytes(&v, channels, u16::MAX), PixelFormat::Rgba16Unorm),
        DecodingResult::F32(v) => (rgba_bytes(&v, channels, 1.0f32), PixelFormat::Rgba32Float),
        _ => return Ok(None),
    };
    Ok(Some(still(
        pixels,
        dims,
        format,
        channels as u8,
        None,
        (Format::Pnm, "PPM"),
    )))
}

/// farbfeld, read here rather than by zune-farbfeld, which (0.5.2) refuses every file: its
/// `decode` sizes the output in bytes and then checks it as a count of `u16`s ("Too small output
/// buffer size"). The format is a 16-byte header — `farbfeld`, then width and height as big-endian
/// `u32` — and big-endian 16-bit RGBA, so all there is to do is swap each sample to native order.
fn decode_farbfeld(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let header = bytes
        .get(..16)
        .filter(|h| h.starts_with(b"farbfeld"))
        .ok_or_else(|| malformed("not a farbfeld header"))?;
    let be32 = |at: usize| {
        u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    let dims = (be32(8) as usize, be32(12) as usize);
    cx.check_dims(dims.0, dims.1, 8, "Farbfeld")?;
    let len = dims.0 * dims.1 * 8;
    let data = bytes
        .get(16..16 + len)
        .ok_or_else(|| malformed("farbfeld pixel data is shorter than its dimensions declare"))?;
    let mut pixels = vec![0u8; len];
    par_chunks_mut(&mut pixels, 8, |offset, part| {
        let src = data[offset..offset + part.len()].as_chunks::<2>().0;
        for (d, s) in part.as_chunks_mut::<2>().0.iter_mut().zip(src) {
            *d = u16::from_be_bytes(*s).to_ne_bytes();
        }
    });
    Ok(still(
        pixels,
        dims,
        PixelFormat::Rgba16Unorm,
        4,
        None,
        (Format::Farbfeld, "Farbfeld"),
    ))
}

/// A still WebP, by whichever decoder is faster for its kind of bitstream; both produce the same
/// pixels. Animated files go to [`decode_zune_image`], which knows how to pick their first frame.
///
/// - **Lossy (VP8)** through libwebp, decoded straight into an RGBA buffer fire owns — a file
///   without alpha gets its opaque lane in the same pass — with libwebp's threaded decoding on:
///   ~0.76 s on an 8192² file where image-webp, the pure-Rust decoder zune wraps, took ~1.6 s.
/// - **Lossless (VP8L)** through image-webp. Lossless decoding is sequential entropy decoding
///   that threads cannot help, and there image-webp measured slightly ahead (~1.01 s vs ~1.10 s).
///
/// Neither decoder's still-image path reads the `ICCP` chunk, so it is read here.
fn decode_webp(bytes: &[u8], cx: &Cx) -> Result<Option<DecodedImage>, DecodeError> {
    use libwebp_sys as webp;

    let status = |s: webp::VP8StatusCode| match s {
        webp::VP8StatusCode::VP8_STATUS_OK => Ok(()),
        e => Err(malformed(format!("WebP: {e:?}"))),
    };
    let mut config =
        webp::WebPDecoderConfig::new().map_err(|_| malformed("libwebp ABI mismatch"))?;
    // SAFETY: `bytes` is a live slice for the call, and `config.input` a valid, initialized
    // struct for libwebp to fill.
    status(unsafe { webp::WebPGetFeatures(bytes.as_ptr(), bytes.len(), &mut config.input) })?;
    if config.input.has_animation != 0 {
        return Ok(None);
    }
    let dims = (config.input.width as usize, config.input.height as usize);
    cx.check_dims(dims.0, dims.1, 4, "WebP")?;
    let channels = if config.input.has_alpha != 0 { 4 } else { 3 };
    let icc = exif::webp_chunk(bytes, b"ICCP").map(<[u8]>::to_vec);
    const LOSSLESS: std::ffi::c_int = 2; // `WebPBitstreamFeatures::format`: 1 lossy, 2 lossless
    if config.input.format == LOSSLESS {
        let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(malformed)?;
        let len = d
            .output_buffer_size()
            .ok_or_else(|| malformed("WebP is too large"))?;
        let mut px = vec![0u8; len];
        d.read_image(&mut px).map_err(malformed)?;
        let pixels = if d.has_alpha() {
            px
        } else {
            rgba_bytes(&px, 3, u8::MAX)
        };
        return Ok(Some(still(
            pixels,
            dims,
            PixelFormat::Rgba8Unorm,
            channels,
            icc,
            (Format::WebP, "WebP"),
        )));
    }

    let mut pixels = vec![0u8; dims.0 * dims.1 * 4];
    config.options.use_threads = 1;
    config.output.colorspace = webp::WEBP_CSP_MODE::MODE_RGBA; // straight, not premultiplied
    config.output.is_external_memory = 1;
    config.output.u.RGBA = webp::WebPRGBABuffer {
        rgba: pixels.as_mut_ptr(),
        stride: (dims.0 * 4) as std::ffi::c_int,
        size: pixels.len(),
    };
    // SAFETY: the output buffer is `pixels`, sized and strided for exactly the dimensions the
    // header reported (libwebp re-checks it against the bitstream and refuses a mismatch), and it
    // outlives the call. With external memory libwebp allocates no output of its own, but
    // `WebPFreeDecBuffer` is its documented release either way.
    let decoded = unsafe {
        let s = webp::WebPDecode(bytes.as_ptr(), bytes.len(), &mut config);
        webp::WebPFreeDecBuffer(&mut config.output);
        s
    };
    status(decoded)?;
    Ok(Some(still(
        pixels,
        dims,
        PixelFormat::Rgba8Unorm,
        channels,
        icc,
        (Format::WebP, "WebP"),
    )))
}

/// JPEG XL through jxl-oxide, at the depth the file was authored in.
///
/// jxl-oxide renders to `f32` in the image's own colour encoding — for an ordinary sRGB file,
/// sRGB-*encoded* values in 0..1, not linear light. The zune wrapper handed those floats over as
/// `Rgba32Float`, which the viewer takes to mean linear/HDR: an 8-bit JPEG XL was shown through
/// the exposure/tonemap path, wrongly, at 16 bytes a pixel. An integer-sample file of up to 8 bits
/// is quantized back to `Rgba8Unorm` (exactly, for a lossless one) and up to 16 bits to
/// `Rgba16Unorm`, both of which the viewer treats as the display-encoded values they are.
///
/// Float-sample and HDR (PQ/HLG) files keep the old `f32` path, as does anything with channels
/// beyond colour and alpha (CMYK's black, spot colours), by returning `None`. Only keyframe 0 is
/// rendered; the zune wrapper rendered every frame of an animation to keep the first.
fn decode_jxl(bytes: &[u8], cx: &Cx) -> Result<Option<DecodedImage>, DecodeError> {
    let img = jxl_oxide::JxlImage::builder()
        .read(Cursor::new(bytes))
        .map_err(malformed)?;
    let dims = (img.width() as usize, img.height() as usize);
    let bits = match img.image_header().metadata.bit_depth {
        jxl_oxide::image::BitDepth::IntegerSample { bits_per_sample } if bits_per_sample <= 16 => {
            bits_per_sample
        }
        _ => return Ok(None),
    };
    let fmt = img.pixel_format();
    if img.hdr_type().is_some() || fmt.has_black() {
        return Ok(None);
    }
    let color = if fmt.is_grayscale() { 1 } else { 3 };
    let channels = color + usize::from(fmt.has_alpha());
    let format = if bits <= 8 {
        PixelFormat::Rgba8Unorm
    } else {
        PixelFormat::Rgba16Unorm
    };
    cx.check_dims(dims.0, dims.1, format.bytes_per_pixel(), "JPEG XL")?;

    let render = img.render_frame(0).map_err(malformed)?;
    // One buffer per channel, orientation applied; alpha follows the colour channels.
    let planes = render.image_planar();
    if planes.len() != channels {
        return Ok(None);
    }
    let planes: Vec<&[f32]> = planes.iter().map(|fb| fb.buf()).collect();
    let n = dims.0 * dims.1;
    if planes.iter().any(|p| p.len() < n) {
        return Err(malformed("JPEG XL render is smaller than the image"));
    }
    let pixels = if bits <= 8 {
        quantize_planes::<4>(&planes, n, 255.0)
    } else {
        quantize_planes::<8>(&planes, n, 65535.0)
    };
    let icc = Some(img.rendered_icc());
    Ok(Some(still(
        pixels,
        dims,
        format,
        channels as u8,
        icc,
        (Format::JpegXl, "JPEG XL"),
    )))
}

/// Planar `f32` (0..1) to interleaved RGBA of `PX` bytes a pixel — 4 for 8-bit samples, 8 for
/// native-endian 16-bit ones — scaled to `max` and rounded to nearest. One plane is grey, two grey+alpha, three RGB, four RGBA.
fn quantize_planes<const PX: usize>(planes: &[&[f32]], n: usize, max: f32) -> Vec<u8> {
    let mut out = vec![0u8; n * PX];
    par_chunks_mut(&mut out, PX, |offset, part| {
        let first = offset / PX;
        for (i, px) in part.as_chunks_mut::<PX>().0.iter_mut().enumerate() {
            let at = first + i;
            let s = |c: usize| planes[c][at];
            let rgba = match planes.len() {
                1 => [s(0), s(0), s(0), 1.0],
                2 => [s(0), s(0), s(0), s(1)],
                3 => [s(0), s(1), s(2), 1.0],
                _ => [s(0), s(1), s(2), s(3)],
            };
            for (c, v) in rgba.into_iter().enumerate() {
                let q = (v.clamp(0.0, 1.0) * max + 0.5) as u16;
                if PX == 4 {
                    px[c] = q as u8;
                } else {
                    px[c * 2..c * 2 + 2].copy_from_slice(&q.to_ne_bytes());
                }
            }
        }
    });
    out
}

/// The generic zune path: `zune_image::Image::read`, normalized to interleaved RGBA in the source
/// bit depth, carrying the embedded ICC profile where the format exposes one. What remains here is
/// what [`decode_zune`]'s direct paths decline — an animated WebP, a float or HDR JPEG XL, a
/// colour layout the direct readers do not widen.
///
/// v1 takes the first frame only (#18: animated GIF → frame 0).
fn decode_zune_image(
    bytes: &[u8],
    fmt: zune_image::codecs::ImageFormat,
    cx: &Cx,
) -> Result<DecodedImage, DecodeError> {
    use zune_core::bit_depth::BitDepth;
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_image::image::Image;

    let opts = zune_options(cx);

    // zune's options cap each *axis* but nothing caps the product, which is what actually gets
    // allocated — so this is the guard that matters, and it has to happen before `Image::read`.
    check_zune_dims(bytes, fmt, opts, cx)?;

    let mut image = Image::read(ZCursor::new(bytes), opts)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;

    // Source characteristics for the status bar, captured before we normalize to RGBA.
    let src_channels = image.colorspace().num_components().min(255) as u8;
    let icc = image.metadata().icc_chunk().cloned();

    // Normalize every colorspace (RGB/Luma/LumaA/CMYK/BGR/…) to interleaved RGBA. This
    // preserves the source bit depth (8/16/float) and adds an opaque alpha where missing.
    image
        .convert_color(ColorSpace::RGBA)
        .map_err(|e| DecodeError::Other(e.to_string()))?;

    let (width, height) = image.dimensions();
    let frame = image
        .frames_ref()
        .first()
        .ok_or_else(|| DecodeError::Malformed("image has no frames".into()))?;

    let (pixels, pixel_format, bit_depth) = match image.depth() {
        BitDepth::Eight => (frame.flatten::<u8>(), PixelFormat::Rgba8Unorm, 8u8),
        BitDepth::Sixteen => {
            // The CPU shader reads Rgba16Unorm back as native-endian u16.
            (
                to_ne_bytes(&frame.flatten::<u16>()),
                PixelFormat::Rgba16Unorm,
                16,
            )
        }
        BitDepth::Float32 => {
            // Float sources are linear/HDR → exposure + tonemap apply.
            (
                to_ne_bytes(&frame.flatten::<f32>()),
                PixelFormat::Rgba32Float,
                32,
            )
        }
        // BitDepth::Unknown and any future variant.
        _ => return Err(DecodeError::Malformed("unsupported bit depth".into())),
    };

    Ok(DecodedImage {
        pixels,
        width: width as u32,
        height: height as u32,
        format: pixel_format,
        source: SourceInfo::new(
            zune_format(fmt),
            zune_format_name(fmt),
            src_channels,
            bit_depth,
        ),
        icc,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    })
}

/// The [`Format`] of a zune-routed stream.
fn zune_format(f: zune_image::codecs::ImageFormat) -> Format {
    use zune_image::codecs::ImageFormat::*;
    match f {
        JPEG => Format::Jpeg,
        PPM => Format::Pnm,
        Farbfeld => Format::Farbfeld,
        QOI => Format::Qoi,
        JPEG_XL => Format::JpegXl,
        BMP => Format::Bmp,
        WEBP => Format::WebP,
        PNG => Format::Png,
        HDR => Format::RadianceHdr,
        _ => Format::Unknown,
    }
}

fn zune_format_name(f: zune_image::codecs::ImageFormat) -> &'static str {
    use zune_image::codecs::ImageFormat::*;
    match f {
        JPEG => "JPEG",
        PPM => "PPM",
        Farbfeld => "Farbfeld",
        QOI => "QOI",
        JPEG_XL => "JPEG XL",
        BMP => "BMP",
        WEBP => "WebP",
        // No PSD arm: `sniff` routes `8BPS` to Backend::Psd before zune is ever consulted.
        _ => "image",
    }
}

/// GIF via the `image` crate. Decodes **every** frame — each already composited to a full RGBA8
/// canvas by the decoder (GIF disposal handled) — so an animated GIF can play; a single-frame GIF
/// comes back as an ordinary still image (`animation: None`). GIF is 8-bit and carries no ICC, so
/// this stays on the simple RGBA8 path. Decode speed is not critical here (GIF is a rare fallback
/// format), and decoding all frames up front keeps the viewer/renderer trivial (a texture swap per
/// frame). Frame 0's pixels are duplicated into `DecodedImage::pixels` so the still-image code paths
/// (first paint, downscale, alpha scan) work unchanged.
fn decode_gif(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    use image::codecs::gif::GifDecoder;
    use image::{AnimationDecoder, ImageDecoder};

    let decoder =
        GifDecoder::new(Cursor::new(bytes)).map_err(|e| DecodeError::Malformed(e.to_string()))?;

    // Two guards, because this backend has two multiplicands. `GifDecoder` is constructed directly
    // (no `ImageReader`, so no default memory limits) and every frame below is decoded to a *full*
    // RGBA canvas: the cost is `frames × w × h × 4`. Bounding the dimensions alone would leave the
    // frame count free to blow past RAM, and bounding the axes alone bounds nothing at all here —
    // GIF's dimensions are `u16`, so even the maximum 65535×65535 is under every per-axis cap while
    // asking for 17 GiB a frame. Hence: one byte-budget check on the canvas, one on the sequence.
    let (w, h) = decoder.dimensions();
    cx.check_dims(w as usize, h as usize, 4, "GIF")?;
    let frame_bytes = (w as u64 * h as u64 * 4).max(1);
    let sequence_bytes = MAX_ANIMATION_BYTES.min(cx.limits().max_bytes);
    let max_frames = match cx.opts.frames {
        Frames::First => 1,
        Frames::All => ((sequence_bytes / frame_bytes) as usize).clamp(1, MAX_ANIMATION_FRAMES),
    };

    // Collected one at a time rather than through `collect_frames`, so the budget can stop the walk
    // rather than discover it too late. Frames past the budget are dropped rather than raising an
    // error — a truncated animation still shows, flagged as such — and no real encoder gets
    // anywhere near the cap.
    let mut frames = Vec::new();
    let mut walk = decoder.into_frames();
    for frame in walk.by_ref() {
        frames.push(frame.map_err(|e| DecodeError::Malformed(e.to_string()))?);
        if frames.len() >= max_frames {
            break;
        }
        if cx.cancelled() {
            return Err(DecodeError::Cancelled);
        }
    }
    // Whether a cap cut the sequence short: there is a frame after the last one kept. `First`
    // asked for one frame, which is not a cut.
    let truncated = cx.opts.frames == Frames::All
        && frames.len() >= max_frames
        && walk.next().is_some_and(|f| f.is_ok());

    let first = frames
        .first()
        .ok_or_else(|| DecodeError::Malformed("GIF has no frames".into()))?;
    let (width, height) = first.buffer().dimensions();

    // Frame 0 pixels for the still path (and the first thing painted).
    let pixels = first.buffer().as_raw().clone();

    // A single-frame GIF is just a still image — skip the animation machinery entirely.
    let animation = (frames.len() > 1).then(|| Animation {
        frames: frames
            .into_iter()
            .map(|f| {
                let (num, den) = f.delay().numer_denom_ms();
                let raw_ms = num.checked_div(den).unwrap_or(0);
                // Browser-compatible clamp: GIFs commonly encode 0 (and sometimes 10 ms) meaning
                // "as fast as possible", which renderers treat as 100 ms. Anything ≥ 20 ms is
                // honored as authored.
                let delay_ms = if raw_ms < 20 { 100 } else { raw_ms };
                AnimationFrame {
                    pixels: f.into_buffer().into_raw(),
                    delay_ms,
                }
            })
            .collect(),
        truncated,
        loop_count: None,
    });

    Ok(DecodedImage {
        pixels,
        width,
        height,
        format: PixelFormat::Rgba8Unorm,
        // GIF is palette-indexed with an optional transparent index → report RGBA (frames can
        // carry transparency); the opaque-alpha scan in `decode` flags the fully-opaque case.
        source: SourceInfo::new(Format::Gif, "GIF", 4, 8),
        icc: None,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation,
    })
}

/// Fallback for the formats zune has no decoder for: TIFF/TGA/ICO via the `image` crate (GIF
/// has its own multi-frame path, see [`decode_gif`]). Extracts the embedded ICC profile (where
/// the format carries one) and keeps float sources as 32-bit float RGBA (HDR). Decode speed here
/// is not critical (rare formats).
///
/// TGA carries no start-of-file magic (only an optional end-of-file `TRUEVISION-XFILE.`
/// footer), so `with_guessed_format` can't detect it from content. Fall back to the file
/// extension for any format content-sniffing misses.
/// A Windows cursor. The container is an ICO with `2` in the type word, and with the directory's
/// "colour planes" and "bits per pixel" fields reused for the hotspot coordinates — neither of
/// which the `image` crate's ICO decoder needs, so the payload reads unchanged. All this does is
/// name the format for the decoder that will not sniff it, and label it honestly afterwards.
fn decode_cur(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let mut img = decode_image(bytes, Some("ico"), cx)?;
    img.source.format = Format::Cur;
    img.source.name = "CUR";
    Ok(img)
}

fn decode_image(
    bytes: &[u8],
    ext_hint: Option<&str>,
    cx: &Cx,
) -> Result<DecodedImage, DecodeError> {
    use image::DynamicImage;

    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| DecodeError::Other(e.to_string()))?;
    if reader.format().is_none()
        && let Some(fmt) = ext_hint.and_then(image::ImageFormat::from_extension)
    {
        reader.set_format(fmt);
    }
    let format = reader.format();
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    // ICC must be queried before the decoder is consumed by from_decoder.
    let (icc, dims, color) = {
        use image::ImageDecoder;
        (
            decoder.icc_profile().ok().flatten(),
            decoder.dimensions(),
            decoder.color_type(),
        )
    };
    // The same byte budget every other backend applies, and the only one on this path: the
    // reader is constructed directly, so the `image` crate's own default memory limits never
    // engage, and `from_decoder` below allocates whatever the header claims.
    cx.check_dims(
        dims.0 as usize,
        dims.1 as usize,
        4 * usize::from(color.bytes_per_pixel() / color.channel_count()).max(1),
        format.map_or("image", format_name),
    )?;
    let dynimg =
        DynamicImage::from_decoder(decoder).map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let width = dynimg.width();
    let height = dynimg.height();
    // Report the source channel count for the status bar and alpha-aware UI (RGBA icon,
    // alpha-channel button, checker backdrop). We always normalize to RGBA below, so the
    // pixel buffer is 4-wide regardless — but a 24-bit TGA / RGB TIFF carries no alpha and
    // must not be presented as if it did.
    let src_channels = dynimg.color().channel_count();

    let (pixels, fmt, bit_depth) = match dynimg {
        // Float sources (Radiance HDR, float TIFF) stay 32-bit float / linear (HDR).
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => {
            (into_rgba32f_bytes(dynimg), PixelFormat::Rgba32Float, 32)
        }
        _ => (into_rgba8(dynimg), PixelFormat::Rgba8Unorm, 8),
    };

    Ok(DecodedImage {
        pixels,
        width,
        height,
        format: fmt,
        source: SourceInfo::new(
            format.map_or(Format::Unknown, image_format),
            format.map_or("image", format_name),
            src_channels,
            bit_depth,
        ),
        icc,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    })
}

/// The [`Format`] of a stream the `image` crate recognised.
fn image_format(f: image::ImageFormat) -> Format {
    use image::ImageFormat as I;
    match f {
        I::Png => Format::Png,
        I::Jpeg => Format::Jpeg,
        I::Gif => Format::Gif,
        I::Bmp => Format::Bmp,
        I::Tiff => Format::Tiff,
        I::Tga => Format::Tga,
        I::Ico => Format::Ico,
        I::WebP => Format::WebP,
        I::Hdr => Format::RadianceHdr,
        I::OpenExr => Format::Exr,
        I::Qoi => Format::Qoi,
        I::Pnm => Format::Pnm,
        I::Farbfeld => Format::Farbfeld,
        I::Dds => Format::Dds,
        I::Avif => Format::Avif,
        _ => Format::Unknown,
    }
}

fn format_name(f: image::ImageFormat) -> &'static str {
    use image::ImageFormat::*;
    match f {
        Png => "PNG",
        Jpeg => "JPEG",
        Gif => "GIF",
        Bmp => "BMP",
        Tiff => "TIFF",
        Tga => "TGA",
        Ico => "ICO",
        WebP => "WebP",
        Hdr => "Radiance HDR",
        _ => "image",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The backend a whole in-memory file routes to.
    fn route_of(bytes: &[u8], ext: Option<&str>) -> Backend {
        route(bytes, bytes.len(), ext)
    }

    /// Encode `img` to the given format with the `image` crate (test fixtures only).
    fn encode(img: &image::DynamicImage, fmt: image::ImageFormat) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, fmt).expect("encode fixture");
        buf.into_inner()
    }

    /// A lossless RGBA PNG must come back byte-for-byte (via the `image`-crate PNG path).
    #[test]
    fn png_rgba_roundtrip() {
        let mut src = image::RgbaImage::new(2, 2);
        src.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        src.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        src.put_pixel(0, 1, image::Rgba([0, 0, 255, 128]));
        src.put_pixel(1, 1, image::Rgba([10, 20, 30, 40]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Png,
        );

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(out.source.name, "PNG");
        assert_eq!(out.source.channels, 4);
        assert_eq!(
            out.pixels,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 128, 10, 20, 30, 40
            ]
        );
    }

    /// A 32-bit RGBA PNG whose alpha is uniformly opaque (e.g. a Windows screenshot) still reports
    /// its true RGBA channel count — the status bar and alpha-channel inspection stay intact — but
    /// is flagged `alpha_opaque` so the viewer doesn't default to the checker backdrop. A single
    /// transparent sample clears the flag. Regression: opaque RGBA screenshots showed transparency.
    #[test]
    fn opaque_rgba_png_flags_alpha_opaque_but_keeps_channels() {
        let mut src = image::RgbaImage::new(2, 1);
        src.put_pixel(0, 0, image::Rgba([200, 30, 40, 255]));
        src.put_pixel(1, 0, image::Rgba([10, 220, 60, 255]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Png,
        );

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(
            out.source.channels, 4,
            "an all-opaque RGBA PNG still reports its true RGBA format"
        );
        assert!(
            out.alpha_opaque,
            "all-opaque alpha => no transparency to composite"
        );
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(&out.pixels[0..4], &[200, 30, 40, 255]);

        // A single transparent pixel makes it genuinely transparent: flag clears.
        let mut src = image::RgbaImage::new(2, 1);
        src.put_pixel(0, 0, image::Rgba([200, 30, 40, 255]));
        src.put_pixel(1, 0, image::Rgba([10, 220, 60, 254]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Png,
        );
        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.source.channels, 4);
        assert!(
            !out.alpha_opaque,
            "a single non-opaque sample is real transparency"
        );
    }

    /// The opaque-alpha flag also covers 16-bit RGBA (alpha at full `0xffff`).
    #[test]
    fn opaque_rgba16_png_flags_alpha_opaque() {
        let mut src = image::ImageBuffer::<image::Rgba<u16>, _>::new(1, 1);
        src.put_pixel(0, 0, image::Rgba([0xFFFF, 0x8000, 0x0001, 0xFFFF]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba16(src),
            image::ImageFormat::Png,
        );

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.format, PixelFormat::Rgba16Unorm);
        assert_eq!(
            out.source.channels, 4,
            "an all-opaque 16-bit RGBA PNG still reports RGBA"
        );
        assert!(out.alpha_opaque, "all-opaque 16-bit alpha => flagged");
    }

    /// A grayscale PNG decodes to Luma then expands to RGBA: gray replicated, opaque alpha.
    /// The reported source channel count stays 1 (for the status bar).
    #[test]
    fn grayscale_png_expands_to_rgba() {
        let mut src = image::GrayImage::new(2, 1);
        src.put_pixel(0, 0, image::Luma([40]));
        src.put_pixel(1, 0, image::Luma([200]));
        let bytes = encode(
            &image::DynamicImage::ImageLuma8(src),
            image::ImageFormat::Png,
        );

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(out.source.channels, 1);
        assert_eq!(out.pixels, vec![40, 40, 40, 255, 200, 200, 200, 255]);
    }

    /// A 16-bit PNG stays 16-bit (precision preserved for the inspector / HDR pipeline).
    #[test]
    fn png16_stays_16bit() {
        let mut src = image::ImageBuffer::<image::Rgba<u16>, _>::new(1, 1);
        src.put_pixel(0, 0, image::Rgba([0xFFFF, 0x8000, 0x0001, 0xFFFF]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba16(src),
            image::ImageFormat::Png,
        );

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.format, PixelFormat::Rgba16Unorm);
        assert_eq!(out.source.bit_depth, 16);
        // Native-endian u16 RGBA.
        let px: Vec<u16> = out
            .pixels
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_ne_bytes(*c))
            .collect();
        assert_eq!(px, vec![0xFFFF, 0x8000, 0x0001, 0xFFFF]);
    }

    /// A PNG in the layouts the `image` encoder cannot write — palette, and 1/2/4-bit — built by
    /// hand: IHDR, then PLTE and tRNS when given, one IDAT holding `rows` (each already packed,
    /// written with filter type 0), and IEND.
    fn raw_png(
        (width, height): (u32, u32),
        depth: u8,
        colour_type: u8,
        plte: &[u8],
        trns: &[u8],
        rows: &[&[u8]],
    ) -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let crc = crc32(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        }
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[depth, colour_type, 0, 0, 0]);
        let raw: Vec<u8> = rows.iter().flat_map(|r| [&[0u8][..], r].concat()).collect();

        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        if !plte.is_empty() {
            chunk(&mut out, b"PLTE", plte);
        }
        if !trns.is_empty() {
            chunk(&mut out, b"tRNS", trns);
        }
        let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        chunk(&mut out, b"IDAT", &idat);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// A palette PNG reports the file's index width and that it is a palette image, while the
    /// buffer is the same expanded RGBA8 as before. `bit_depth` used to say 8 for every one.
    #[test]
    fn palette_png_reports_its_index_depth() {
        // Three colours, indices 0 1 2 packed two to a byte; the last nibble is padding.
        let plte = [255, 0, 0, 0, 255, 0, 0, 0, 255];
        let bytes = raw_png((3, 1), 4, 3, &plte, &[], &[&[0x01, 0x20]]);

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.source.bit_depth, out.source.palette), (4, true));
        assert_eq!(out.source.channels, 3);
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(
            out.pixels,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]
        );
    }

    /// An 8-bit palette with transparency (tRNS) expands to RGBA and still reads as a palette.
    #[test]
    fn palette_png_with_transparency_keeps_its_alpha() {
        let plte = [10, 20, 30, 40, 50, 60];
        let bytes = raw_png((2, 1), 8, 3, &plte, &[128], &[&[0, 1]]);

        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.source.bit_depth, out.source.palette), (8, true));
        assert_eq!(out.source.channels, 4);
        assert_eq!(out.pixels, vec![10, 20, 30, 128, 40, 50, 60, 255]);
        assert!(!out.alpha_opaque);
    }

    /// Low-depth greyscale reports its own depth too, scaled to full range in the buffer.
    #[test]
    fn low_depth_grey_png_reports_its_depth() {
        let one = raw_png((4, 1), 1, 0, &[], &[], &[&[0b1010_0000]]);
        let out = decode(&one, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(
            (
                out.source.bit_depth,
                out.source.palette,
                out.source.channels
            ),
            (1, false, 1)
        );
        let grey: Vec<u8> = out.pixels.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(grey, vec![255, 0, 255, 0]);

        let two = raw_png((4, 1), 2, 0, &[], &[], &[&[0b00_01_10_11]]);
        let out = decode(&two, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.source.bit_depth, out.source.palette), (2, false));
        let grey: Vec<u8> = out.pixels.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        assert_eq!(grey, vec![0, 85, 170, 255]);
    }

    /// An ordinary truecolour PNG is unchanged: 8 bits, no palette.
    #[test]
    fn truecolour_png_is_not_a_palette() {
        let bytes = raw_png((1, 1), 8, 2, &[], &[], &[&[1, 2, 3]]);
        let out = decode(&bytes, Some("png"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(
            (
                out.source.bit_depth,
                out.source.palette,
                out.source.channels
            ),
            (8, false, 3)
        );
        assert_eq!(out.pixels, vec![1, 2, 3, 255]);
    }

    /// A PNG's embedded ICC profile is surfaced by the `image`-crate PNG path (zune-png
    /// surfaced it via its metadata chunk; the routing change must not lose it).
    #[cfg(feature = "icc")] // lcms2 makes the profile
    #[test]
    fn png_icc_profile_is_surfaced() {
        use image::ImageEncoder;

        let icc = lcms2::Profile::new_srgb().icc().unwrap();
        let mut buf = Vec::new();
        let mut enc = image::codecs::png::PngEncoder::new(&mut buf);
        enc.set_icc_profile(icc.clone())
            .expect("png encoder supports ICC");
        enc.write_image(&[200u8, 30, 40, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode fixture");

        // honor_icc=false so the raw profile bytes survive for the assertion.
        let opts = DecodeOptions {
            #[cfg(feature = "icc")]
            honor_icc: false,
            ..DecodeOptions::viewer()
        };
        let out = decode(&buf, Some("png"), &opts).unwrap();
        assert_eq!(out.source.name, "PNG");
        assert_eq!(
            out.icc.as_deref(),
            Some(icc.as_slice()),
            "embedded ICC must be surfaced"
        );
    }

    /// A solid-color JPEG decodes through zune to RGBA8 with the right dims (lossy, so we
    /// only assert the color is approximately right, and source is reported as 3-channel).
    #[test]
    fn zune_jpeg_decodes() {
        let src = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            16,
            16,
            image::Rgb([220, 30, 40]),
        ));
        let bytes = encode(&src, image::ImageFormat::Jpeg);

        let out = decode(&bytes, Some("jpg"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.width, out.height), (16, 16));
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(out.source.name, "JPEG");
        assert_eq!(out.source.channels, 3);
        let [r, g, b, a] = [out.pixels[0], out.pixels[1], out.pixels[2], out.pixels[3]];
        assert!(r > 200 && g < 70 && b < 80, "got {r},{g},{b}");
        assert_eq!(a, 255);
    }

    /// An ICO routes to the `image`-crate fallback (zune can't sniff it) and decodes to
    /// RGBA8 with the right dims and status-bar label. ICO embeds a PNG or BMP per entry;
    /// the `image` crate picks the largest. Proves the AVIF/HEIF work didn't need to touch
    /// ICO — it already works through the fallback.
    #[test]
    fn ico_decodes_via_fallback() {
        let mut src = image::RgbaImage::new(4, 4);
        src.put_pixel(0, 0, image::Rgba([200, 30, 40, 255]));
        src.put_pixel(3, 3, image::Rgba([10, 20, 30, 128]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Ico,
        );

        let out = decode(&bytes, Some("ico"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.width, out.height), (4, 4));
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(out.source.name, "ICO");
        assert_eq!(
            [out.pixels[0], out.pixels[1], out.pixels[2], out.pixels[3]],
            [200, 30, 40, 255]
        );
    }

    /// A Windows cursor is an ICO whose type word is 2 instead of 1, with two directory fields
    /// reused for the hotspot. The `image` crate reads the payload unchanged but will not sniff
    /// the magic, so `sniff` names the format for it — and the status bar says CUR, not ICO.
    ///
    /// The `sniff` assertion is also what keeps the TGA disambiguation honest: an uncompressed
    /// true-colour TGA shares this magic (see `uncompressed_truecolor_tga_is_not_a_cursor`) and
    /// telling the two apart must not cost us the cursor.
    #[test]
    fn cur_decodes_through_the_ico_decoder_and_is_labelled_cur() {
        let mut src = image::RgbaImage::new(4, 4);
        src.put_pixel(0, 0, image::Rgba([200, 30, 40, 255]));
        let mut bytes = encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Ico,
        );
        // Bytes 2..4 are the ICONDIR type: 1 = icon, 2 = cursor. Everything else is identical.
        bytes[2] = 2;
        bytes[3] = 0;

        assert!(matches!(route_of(&bytes, None), Backend::Cur));
        let out = decode(&bytes, Some("cur"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.width, out.height), (4, 4));
        assert_eq!(out.source.name, "CUR");
        assert_eq!(
            [out.pixels[0], out.pixels[1], out.pixels[2], out.pixels[3]],
            [200, 30, 40, 255]
        );
    }

    /// Netpbm's `P7` (Portable Arbitrary Map) is the one member of the family the extension
    /// table did not list, though the decoders have always read it.
    #[test]
    fn pam_decodes() {
        let mut bytes =
            b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 3\nMAXVAL 255\nTUPLTYPE RGB\nENDHDR\n".to_vec();
        bytes.extend_from_slice(&[200, 30, 40, 10, 20, 30]);

        let out = decode(&bytes, Some("pam"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(out.source.channels, 3);
        assert_eq!(out.pixels[..8], [200, 30, 40, 255, 10, 20, 30, 255]);
    }

    /// The Radiance aliases carry the same magic as `.hdr`, so listing them is purely about the
    /// extension table — nothing about the routing changes.
    #[test]
    fn radiance_aliases_are_listed_and_route_to_the_hdr_backend() {
        for ext in ["hdr", "pic", "rgbe", "xyze"] {
            assert!(is_supported_extension(ext), ".{ext} is listed");
        }
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81".to_vec();
        assert!(matches!(route_of(&bytes, Some("pic")), Backend::Hdr));
    }

    /// DDS is sniffed by its own magic ahead of every fallback, and `.tx` (an OpenImageIO tiled
    /// TIFF) still reaches the TIFF backend by TIFF's magic.
    #[test]
    fn dds_and_tx_route_to_the_right_backends() {
        assert!(matches!(
            route_of(b"DDS \x7c\x00\x00\x00", None),
            Backend::Dds
        ));
        assert!(matches!(
            route_of(b"II\x2a\x00rest", Some("tx")),
            Backend::Tiff
        ));
        for ext in ["dds", "cur", "pam", "tx"] {
            assert!(is_supported_extension(ext), ".{ext} is listed");
        }
    }

    /// A camera-raw file routes to the preview extractor: a synthetic little-endian TIFF
    /// whose IFD points at an embedded full-size JPEG (with Orientation 8 / rotate-90°-CCW)
    /// decodes to the preview's pixels, re-labeled as the raw family, oriented upright. This
    /// exercises the whole wiring: ext-driven sniff -> decode_raw -> zune -> orientation.
    #[test]
    fn raw_decodes_embedded_preview_via_ext() {
        // Embedded preview: a 6(w)x2(h) JPEG. Orientation 8 swaps axes -> 2x6 displayed.
        let preview = encode(
            &image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                6,
                2,
                image::Rgb([200, 40, 60]),
            )),
            image::ImageFormat::Jpeg,
        );

        let entries: u16 = 3;
        let ifd_off = 8u32;
        let ifd_len = 2 + entries as usize * 12 + 4;
        let jpeg_off = ifd_off as usize + ifd_len;

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&ifd_off.to_le_bytes());
        tiff.extend_from_slice(&entries.to_le_bytes());
        let mut entry = |tag: u16, typ: u16, n: u32, val: u32| {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&typ.to_le_bytes());
            tiff.extend_from_slice(&n.to_le_bytes());
            tiff.extend_from_slice(&val.to_le_bytes());
        };
        entry(0x0112, 3, 1, 8); // Orientation = 8 (rotate 90° CCW)
        entry(0x0201, 4, 1, jpeg_off as u32);
        entry(0x0202, 4, 1, preview.len() as u32);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&preview);

        let result = decode(&tiff, Some("nef"), &DecodeOptions::viewer());
        if !cfg!(feature = "raw") {
            // Routed as raw all the same, and refused: the bytes are a raw, not a TIFF.
            assert!(matches!(
                result,
                Err(DecodeError::Unsupported(Format::CameraRaw))
            ));
            return;
        }
        let out = result.unwrap();
        assert_eq!(out.source.name, "Nikon NEF");
        assert_eq!(out.source.format, Format::CameraRaw);
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        // Orientation 8 swaps the 6x2 preview to 2x6.
        assert_eq!((out.width, out.height), (2, 6));
        // The camera's red is preserved through the JPEG round-trip.
        let [r, g, b] = [out.pixels[0], out.pixels[1], out.pixels[2]];
        assert!(r > 180 && g < 90 && b < 100, "got {r},{g},{b}");
        // With EXIF orientation off, the preview stays as stored.
        let stored = decode(&tiff, Some("nef"), &DecodeOptions::inspect()).unwrap();
        assert_eq!((stored.width, stored.height), (6, 2));
    }

    /// The end-to-end EXIF-orientation path for an ordinary photo: a JPEG whose `APP1` says
    /// "rotate 90° CW" comes back with its axes swapped and its content actually turned, not just
    /// relabeled. No decoder in the stack does this for us — zune, `image`, `tiff` and `exr` all
    /// hand back the stored pixels — so without [`exif`] every portrait phone photo displayed on
    /// its side. Regression test for exactly that.
    #[test]
    fn jpeg_exif_orientation_is_honored() {
        // 16 wide, 8 tall: red left half, blue right half.
        let mut src = image::RgbImage::new(16, 8);
        for (x, _y, p) in src.enumerate_pixels_mut() {
            *p = if x < 8 {
                image::Rgb([220, 30, 30])
            } else {
                image::Rgb([30, 30, 220])
            };
        }
        let plain = encode(
            &image::DynamicImage::ImageRgb8(src),
            image::ImageFormat::Jpeg,
        );

        // An `APP1` EXIF segment (a bare IFD0 holding Orientation = 6) spliced in after the SOI,
        // which is where a camera writes it.
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(b"II\x2a\x00");
        app1.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8
        app1.extend_from_slice(&1u16.to_le_bytes()); // one entry
        app1.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        app1.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        app1.extend_from_slice(&1u32.to_le_bytes()); // count
        app1.extend_from_slice(&6u32.to_le_bytes()); // 6 = rotate 90° CW
        app1.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
        bytes.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(&app1);
        bytes.extend_from_slice(&plain[2..]);

        // Untagged, the JPEG is 16x8 with red on the left.
        let plain_out = decode(&plain, Some("jpg"), &DecodeOptions::viewer()).unwrap();
        assert_eq!((plain_out.width, plain_out.height), (16, 8));

        let out = decode(&bytes, Some("jpg"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.source.name, "JPEG");
        assert_eq!(
            out.source.channels, 3,
            "rotating must not invent an alpha channel"
        );
        assert_eq!(
            (out.width, out.height),
            (8, 16),
            "orientation 6 swaps the axes"
        );
        // Rotated 90° CW, the red left half is now the top half: first row red, last row blue.
        let px = |x: u32, y: u32| {
            let i = ((y * out.width + x) * 4) as usize;
            [out.pixels[i], out.pixels[i + 1], out.pixels[i + 2]]
        };
        let [r, g, b] = px(0, 0);
        assert!(
            r > 180 && g < 90 && b < 90,
            "top row should be red: {r},{g},{b}"
        );
        let [r, g, b] = px(0, 15);
        assert!(
            b > 180 && r < 90 && g < 90,
            "bottom row should be blue: {r},{g},{b}"
        );
    }

    /// A format zune sniffs but then fails to decode falls through to the `image` crate.
    ///
    /// zune-bmp cannot read a 24-bit BMP narrower than 3 pixels — a 1x1 or 2x1 colour swatch
    /// errored with "Not enough bytes" and the file would not open at all. zune stays the hot
    /// path; this only runs once it has already said no.
    #[test]
    fn zune_failure_falls_back_to_the_image_crate() {
        for (w, h) in [(1u32, 1u32), (2, 1), (3, 1), (8, 8)] {
            let src = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                w,
                h,
                image::Rgb([200, 30, 40]),
            ));
            let bytes = encode(&src, image::ImageFormat::Bmp);
            let out = decode(&bytes, Some("bmp"), &DecodeOptions::viewer())
                .unwrap_or_else(|e| panic!("{w}x{h} BMP must open: {e}"));
            assert_eq!((out.width, out.height), (w, h));
            assert_eq!(&out.pixels[0..4], &[200, 30, 40, 255], "{w}x{h}");
            assert_eq!(out.source.channels, 3);
        }
    }

    /// A file neither decoder can read still reports zune's error, not the fallback's.
    #[test]
    fn zune_fallback_keeps_the_original_error() {
        // A BMP header zune sniffs, followed by nothing either decoder can use.
        let mut junk = b"BM".to_vec();
        junk.extend_from_slice(&[0u8; 40]);
        assert!(decode(&junk, Some("bmp"), &DecodeOptions::viewer()).is_err());
    }

    /// The fallback must never be a way around a decode guard.
    ///
    /// `check_dims` refusing an image and a decoder failing to read one look the same from the
    /// outside, and when they did, the fallback happily handed a 65535x65535 JPEG that zune had
    /// correctly refused to the `image` crate — which allocated it. A guard rejection is
    /// [`DecodeError::TooLarge`] and stops there; only a real decode failure gets a second try.
    #[test]
    fn a_guard_rejection_is_never_retried_on_the_fallback() {
        // A real 8x8 JPEG with its SOF0 dimensions overwritten to 65535x65535, exactly as
        // `zune_jpeg_decode_bomb_is_rejected_by_the_byte_budget` builds it.
        let mut jpg = encode(
            &image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8)),
            image::ImageFormat::Jpeg,
        );
        let sof = jpg
            .windows(2)
            .position(|w| w == [0xFF, 0xC0])
            .expect("baseline JPEG has an SOF0");
        jpg[sof + 5..sof + 7].copy_from_slice(&u16::MAX.to_be_bytes()); // height
        jpg[sof + 7..sof + 9].copy_from_slice(&u16::MAX.to_be_bytes()); // width

        let err = decode(&jpg, Some("jpg"), &DecodeOptions::viewer())
            .expect_err("a decode bomb must be refused");
        assert!(
            matches!(err, DecodeError::TooLarge(_)),
            "a guard rejection must stay TooLarge so the fallback declines it, got {err:?}"
        );
    }

    /// The threaded, block-wise opacity scan finds a lone transparent pixel wherever it is — first,
    /// last, either side of a block boundary, at the start of the last thread's part — in every
    /// format, and calls an all-opaque buffer opaque.
    #[test]
    fn alpha_scan_finds_one_transparent_pixel_anywhere() {
        let n = 1_000_003usize; // ~4–16 MB: several threads, a ragged last block
        for (format, opaque, clear) in [
            (
                PixelFormat::Rgba8Unorm,
                vec![9, 9, 9, 0xff],
                vec![9, 9, 9, 0xfe],
            ),
            (
                PixelFormat::Rgba16Unorm,
                to_ne_bytes(&[1u16, 2, 3, 0xffff]),
                to_ne_bytes(&[1u16, 2, 3, 0xff00]),
            ),
            (
                PixelFormat::Rgba16Float,
                to_ne_bytes(&[1u16, 2, 3, 0x3c00]),
                to_ne_bytes(&[1u16, 2, 3, 0x3bff]),
            ),
            (
                PixelFormat::Rgba32Float,
                to_ne_bytes(&[0.5f32, 2.0, 3.0, 1.5]),
                to_ne_bytes(&[0.5f32, 2.0, 3.0, f32::NAN]),
            ),
        ] {
            let bpp = opaque.len();
            let mut img = still(
                opaque.repeat(n),
                (n, 1),
                format,
                4,
                None,
                (Format::Unknown, "test"),
            );
            assert!(alpha_is_opaque(&img), "{format:?} all opaque");
            let block = 64 * 1024 / bpp;
            let part = (n * bpp).div_ceil(8).div_ceil(64 * 1024) * block;
            for at in [0, block - 1, block, n / 2, part * 7, n - 1] {
                img.pixels[at * bpp..(at + 1) * bpp].copy_from_slice(&clear);
                assert!(!alpha_is_opaque(&img), "{format:?} transparent at {at}");
                img.pixels[at * bpp..(at + 1) * bpp].copy_from_slice(&opaque);
            }
        }
    }

    /// WebP routes each still by bitstream kind — lossy to libwebp, lossless to image-webp — and
    /// both must give exactly what image-webp, as an independent reference, decodes: straight
    /// (not premultiplied) alpha, an opaque lane added where the file has none, and the file's
    /// own channel count. The alpha ramps across each row so premultiplying would show; 7×5, so
    /// no row is a multiple of any SIMD width. (ImageMagick-encoded, from a crop of a test image.)
    #[test]
    fn webp_stills_match_the_reference_decoder() {
        const LOSSY: &[u8] = &[
            0x52, 0x49, 0x46, 0x46, 0x42, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50,
            0x38, 0x20, 0x36, 0x00, 0x00, 0x00, 0xd0, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x07, 0x00,
            0x05, 0x00, 0x01, 0x00, 0x1c, 0x25, 0x98, 0x02, 0x74, 0x00, 0xf9, 0x8a, 0xc8, 0x85,
            0x60, 0x00, 0xfe, 0xf3, 0x40, 0xaf, 0x65, 0xff, 0x45, 0xb0, 0x66, 0x48, 0x79, 0x42,
            0xac, 0xd7, 0xfe, 0x55, 0x47, 0x97, 0xfd, 0x82, 0xff, 0xa9, 0x99, 0x07, 0x1f, 0x04,
            0xdd, 0x38, 0x00, 0x00,
        ];
        const LOSSY_ALPHA: &[u8] = &[
            0x52, 0x49, 0x46, 0x46, 0x78, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50,
            0x38, 0x58, 0x0a, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x04,
            0x00, 0x00, 0x41, 0x4c, 0x50, 0x48, 0x1c, 0x00, 0x00, 0x00, 0x01, 0x37, 0x20, 0x10,
            0x48, 0xe1, 0x30, 0x1d, 0x11, 0x91, 0x6d, 0x90, 0x65, 0x23, 0x6d, 0x37, 0x1f, 0xcb,
            0xb1, 0xbc, 0xbf, 0xce, 0x2b, 0x44, 0xf4, 0x3f, 0x6c, 0x7c, 0x56, 0x50, 0x38, 0x20,
            0x36, 0x00, 0x00, 0x00, 0xd0, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x07, 0x00, 0x05, 0x00,
            0x01, 0x00, 0x1c, 0x25, 0x98, 0x02, 0x74, 0x00, 0xf9, 0x8a, 0x67, 0xb5, 0x00, 0x00,
            0xfe, 0xf5, 0xb9, 0x9a, 0xf5, 0xc9, 0xd0, 0xe9, 0xc0, 0x1b, 0x3d, 0x85, 0xa4, 0x99,
            0xbf, 0xf9, 0x4b, 0x23, 0x9f, 0xe9, 0x27, 0xfb, 0x1b, 0xd4, 0xe0, 0x00, 0x9b, 0xb0,
            0x30, 0x00,
        ];
        const LOSSLESS: &[u8] = &[
            0x52, 0x49, 0x46, 0x46, 0x74, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50,
            0x38, 0x4c, 0x68, 0x00, 0x00, 0x00, 0x2f, 0x06, 0x00, 0x01, 0x10, 0xb5, 0x30, 0x68,
            0x24, 0x49, 0x91, 0xc1, 0x63, 0xe6, 0xbb, 0x67, 0x7e, 0xff, 0x52, 0x46, 0x8a, 0x80,
            0xa0, 0x44, 0xc2, 0x20, 0x00, 0xc0, 0x32, 0xfd, 0xeb, 0xcc, 0xb6, 0x79, 0x14, 0x22,
            0x08, 0x20, 0x91, 0x49, 0x26, 0x98, 0x65, 0x86, 0x59, 0x66, 0x99, 0xc0, 0x0b, 0x71,
            0x2e, 0x1d, 0x5a, 0x9c, 0xaf, 0x51, 0xdf, 0x96, 0x3f, 0xfe, 0x00, 0x07, 0x6d, 0x78,
            0xc7, 0xba, 0x9d, 0xaa, 0x9b, 0xc5, 0xe3, 0x42, 0x58, 0x39, 0xa5, 0x2f, 0xca, 0xe9,
            0x24, 0x6e, 0x52, 0x8e, 0x9d, 0xa2, 0x48, 0x32, 0x5f, 0x94, 0xd3, 0x49, 0x58, 0x54,
            0x8e, 0xf5, 0x80, 0x6b, 0x49, 0xfa, 0xa3, 0x7a, 0x5b, 0xb4, 0x39, 0xfd,
        ];
        for (name, bytes, channels) in [
            ("lossy", LOSSY, 3),
            ("lossy + alpha", LOSSY_ALPHA, 4),
            ("lossless + alpha", LOSSLESS, 4),
        ] {
            let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).unwrap();
            let mut buf = vec![0; d.output_buffer_size().unwrap()];
            d.read_image(&mut buf).unwrap();
            let expected = if d.has_alpha() {
                buf
            } else {
                rgba_bytes(&buf, 3, u8::MAX)
            };
            let out = decode(bytes, Some("webp"), &DecodeOptions::viewer()).unwrap();
            assert_eq!(
                (out.width, out.height, out.source.channels, out.source.name),
                (7, 5, channels, "WebP"),
                "{name}"
            );
            assert_eq!(out.pixels, expected, "{name}");
            if channels == 4 {
                assert!(out.pixels.as_chunks::<4>().0.iter().any(|p| p[3] < 255));
            }
        }
    }

    /// QOI round-trips through the `qoi` crate's decoder in both of its layouts: RGBA as stored,
    /// RGB with an opaque alpha added, and the file's channel count reported either way.
    #[test]
    fn qoi_decodes_rgb_and_rgba() {
        let (w, h) = (13u32, 7u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 % 251) as u8).collect();
        let rgb: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        for (src, channels) in [(&rgba, 4u8), (&rgb, 3)] {
            let bytes = qoi::encode_to_vec(src, w, h).unwrap();
            let out = decode(&bytes, Some("qoi"), &DecodeOptions::viewer()).unwrap();
            assert_eq!(
                (out.width, out.height, out.source.channels),
                (w, h, channels)
            );
            assert_eq!(out.source.name, "QOI");
            let expected = if channels == 4 {
                rgba.clone()
            } else {
                rgba_bytes(&rgb, 3, u8::MAX)
            };
            assert_eq!(out.pixels, expected, "{channels} channels");
        }
    }

    /// farbfeld (which zune-farbfeld refuses outright) reads as big-endian 16-bit RGBA, and a file
    /// cut short of its declared pixels is refused rather than read past.
    #[test]
    fn farbfeld_decodes_big_endian_rgba16() {
        let samples: [u16; 8] = [0x0102, 0xfffe, 0, 65535, 300, 40_000, 1, 0x8000];
        let mut bytes = b"farbfeld".to_vec();
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        for s in samples {
            bytes.extend_from_slice(&s.to_be_bytes());
        }
        let out = decode(&bytes, Some("ff"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(
            (out.width, out.height, out.format),
            (2, 1, PixelFormat::Rgba16Unorm)
        );
        assert_eq!(out.source.name, "Farbfeld");
        assert_eq!(out.pixels, to_ne_bytes(&samples));
        assert!(
            decode(
                &bytes[..bytes.len() - 1],
                Some("ff"),
                &DecodeOptions::viewer()
            )
            .is_err()
        );
    }

    /// A minimal uncompressed PSD: header, three empty sections, then the merged image as raw
    /// big-endian planes (`samples` holds plane 0, then plane 1, …).
    #[cfg(feature = "psd")]
    fn raw_psd(w: u32, h: u32, channels: u16, depth: u16, mode: u16, samples: &[u16]) -> Vec<u8> {
        let mut v = b"8BPS".to_vec();
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&[0; 6]);
        v.extend_from_slice(&channels.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&depth.to_be_bytes());
        v.extend_from_slice(&mode.to_be_bytes());
        v.extend_from_slice(&[0; 12]); // colour mode data, image resources, layers: all empty
        v.extend_from_slice(&0u16.to_be_bytes()); // raw, uncompressed
        for &s in samples {
            if depth == 8 {
                v.push(s as u8);
            } else {
                v.extend_from_slice(&s.to_be_bytes());
            }
        }
        v
    }

    /// The PSD wrapper's integer fast path (8/16-bit RGB and greyscale): every sample comes out
    /// exactly as stored, at 16 bits too — a PSD's 16-bit samples are full-range 0..65535
    /// (Photoshop's 15-bit+1 is internal only; it scales on save), and reading them as 0..32768
    /// clipped everything above mid-scale to white. A missing alpha is opaque. Large enough to be
    /// split across threads, with every 16-bit value covered.
    #[cfg(feature = "psd")]
    #[test]
    fn psd_integer_fast_path_keeps_samples_as_stored() {
        let (w, h) = (1031u32, 523u32);
        let n = (w * h) as usize;
        let plane = |seed: usize| -> Vec<u16> {
            (0..n)
                .map(|i| ((i * 7 + seed * 13_331) % 65536) as u16)
                .collect()
        };

        // 8-bit RGBA.
        let planes: Vec<Vec<u16>> = (0..4)
            .map(|c| plane(c).iter().map(|v| v & 0xff).collect())
            .collect();
        let out = decode(
            &raw_psd(w, h, 4, 8, 3, &planes.concat()),
            Some("psd"),
            &DecodeOptions::viewer(),
        )
        .unwrap();
        assert_eq!(
            (out.format, out.source.channels),
            (PixelFormat::Rgba8Unorm, 4)
        );
        let expected: Vec<u8> = (0..n)
            .flat_map(|i| planes.iter().map(move |p| p[i] as u8))
            .collect();
        assert!(out.pixels == expected, "8-bit RGBA");

        // 16-bit RGB: no alpha plane, so alpha is opaque.
        let planes: Vec<Vec<u16>> = (0..3).map(plane).collect();
        let out = decode(
            &raw_psd(w, h, 3, 16, 3, &planes.concat()),
            Some("psd"),
            &DecodeOptions::viewer(),
        )
        .unwrap();
        assert_eq!(
            (out.format, out.source.channels),
            (PixelFormat::Rgba16Unorm, 3)
        );
        let expected: Vec<u16> = (0..n)
            .flat_map(|i| [planes[0][i], planes[1][i], planes[2][i], 65535])
            .collect();
        assert!(out.pixels == to_ne_bytes(&expected), "16-bit RGB");

        // 16-bit greyscale + alpha: grey to all three, the second plane as alpha.
        let planes: Vec<Vec<u16>> = (0..2).map(plane).collect();
        let out = decode(
            &raw_psd(w, h, 2, 16, 1, &planes.concat()),
            Some("psd"),
            &DecodeOptions::viewer(),
        )
        .unwrap();
        let expected: Vec<u16> = (0..n)
            .flat_map(|i| {
                let g = planes[0][i];
                [g, g, g, planes[1][i]]
            })
            .collect();
        assert!(out.pixels == to_ne_bytes(&expected), "16-bit grey + alpha");
    }

    /// TGA has no start-of-file magic, so content sniffing can't identify it — the decoder
    /// must lean on the file extension. Regression for TGA files failing to open at all.
    #[test]
    fn tga_decodes_via_extension_hint() {
        let mut src = image::RgbaImage::new(2, 1);
        src.put_pixel(0, 0, image::Rgba([200, 30, 40, 255]));
        src.put_pixel(1, 0, image::Rgba([10, 220, 60, 255]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Tga,
        );

        // Without an extension hint the format is unidentifiable (the original bug).
        assert!(decode(&bytes, None, &DecodeOptions::viewer()).is_err());

        // With the hint it decodes — and the hint is matched case-insensitively (.TGA).
        for ext in ["tga", "TGA"] {
            let out = decode(&bytes, Some(ext), &DecodeOptions::viewer()).unwrap();
            assert_eq!((out.width, out.height), (2, 1));
            assert_eq!(out.source.name, "TGA");
            assert_eq!(&out.pixels[0..4], &[200, 30, 40, 255]);
        }
    }

    /// A 24-bit (RGB) TGA carries no alpha; it must report 3 channels so the viewer doesn't
    /// present it with a checker backdrop / alpha-channel UI. Regression: `decode_image`
    /// hardcoded `channels: 4`, so every TGA looked like it had an alpha channel.
    #[test]
    fn rgb_tga_reports_three_channels() {
        let mut src = image::RgbImage::new(2, 1);
        src.put_pixel(0, 0, image::Rgb([200, 30, 40]));
        src.put_pixel(1, 0, image::Rgb([10, 220, 60]));
        let bytes = encode(
            &image::DynamicImage::ImageRgb8(src),
            image::ImageFormat::Tga,
        );

        let out = decode(&bytes, Some("tga"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(
            out.source.channels, 3,
            "24-bit RGB TGA must not report an alpha channel"
        );
        // Still normalized to RGBA pixels with opaque alpha for the GPU upload.
        assert_eq!(&out.pixels[0..4], &[200, 30, 40, 255]);

        // A genuine 32-bit RGBA TGA still reports 4 channels.
        let mut rgba = image::RgbaImage::new(1, 1);
        rgba.put_pixel(0, 0, image::Rgba([1, 2, 3, 128]));
        let bytes = encode(
            &image::DynamicImage::ImageRgba8(rgba),
            image::ImageFormat::Tga,
        );
        let out = decode(&bytes, Some("tga"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(
            out.source.channels, 4,
            "32-bit RGBA TGA must report an alpha channel"
        );
    }

    /// `into_rgba8` / `into_rgba16_bytes` / `into_rgba32f_bytes` widen with `rgba_bytes` instead
    /// of the `image` crate's conversions (whose float round trip made an 8192² greyscale TGA
    /// decode ~4× slower than the RGB one). They must produce exactly what the crate would have,
    /// for every layout they take over, at every depth — including the extremes, where a float
    /// round trip would be the first to drift. The image is big enough to be split across threads
    /// and sized so that no part boundary falls on a whole row.
    #[test]
    fn rgba_widening_matches_the_image_crate() {
        use image::{DynamicImage, ImageBuffer, Luma, LumaA, Rgb, Rgba};

        let (w, h) = (1031u32, 263u32);
        let v8 = |i: u32| (i.wrapping_mul(97) % 256) as u8;
        let v16 = |i: u32| i.wrapping_mul(40_503) as u16;
        let vf = |i: u32| (i % 1000) as f32 / 300.0 - 0.5;
        let at = |x: u32, y: u32| y * w + x;

        let eight = [
            DynamicImage::ImageLuma8(ImageBuffer::from_fn(w, h, |x, y| Luma([v8(at(x, y))]))),
            DynamicImage::ImageLumaA8(ImageBuffer::from_fn(w, h, |x, y| {
                LumaA([v8(at(x, y)), v8(x * 7 + y)])
            })),
            DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
                Rgb([v8(at(x, y)), v8(x + 3), v8(y * 5)])
            })),
            DynamicImage::ImageRgba8(ImageBuffer::from_fn(w, h, |x, y| {
                Rgba([v8(at(x, y)), v8(x + 3), v8(y * 5), v8(x ^ y)])
            })),
        ];
        for img in eight {
            let expected = img.to_rgba8().into_raw();
            assert!(into_rgba8(img.clone()) == expected, "{:?}", img.color());
        }
        let sixteen = [
            DynamicImage::ImageLuma16(ImageBuffer::from_fn(w, h, |x, y| Luma([v16(at(x, y))]))),
            DynamicImage::ImageLumaA16(ImageBuffer::from_fn(w, h, |x, y| {
                LumaA([v16(at(x, y)), v16(x * 7 + y)])
            })),
            DynamicImage::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
                Rgb([v16(at(x, y)), v16(x + 3), v16(y * 5)])
            })),
            DynamicImage::ImageRgba16(ImageBuffer::from_fn(w, h, |x, y| {
                Rgba([v16(at(x, y)), v16(x + 3), v16(y * 5), v16(x ^ y)])
            })),
        ];
        for img in sixteen {
            let expected = to_ne_bytes(img.to_rgba16().as_raw());
            assert!(
                into_rgba16_bytes(img.clone()) == expected,
                "{:?}",
                img.color()
            );
        }
        let float = [
            DynamicImage::ImageRgb32F(ImageBuffer::from_fn(w, h, |x, y| {
                Rgb([vf(at(x, y)), vf(x + 3), vf(y * 5)])
            })),
            DynamicImage::ImageRgba32F(ImageBuffer::from_fn(w, h, |x, y| {
                Rgba([vf(at(x, y)), vf(x + 3), vf(y * 5), vf(x ^ y)])
            })),
        ];
        for img in float {
            let expected = to_ne_bytes(img.to_rgba32f().as_raw());
            assert!(
                into_rgba32f_bytes(img.clone()) == expected,
                "{:?}",
                img.color()
            );
        }
    }

    /// A greyscale TGA (image type 3) decodes through the fast grey expansion end to end: one
    /// source channel, the grey sample replicated across RGB, opaque alpha.
    #[test]
    fn grey_tga_expands_to_opaque_rgba() {
        let mut bytes = vec![0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1, 0, 8, 0x20];
        bytes.extend_from_slice(&[17, 230]);
        let out = decode(&bytes, Some("tga"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.source.name, "TGA");
        assert_eq!(out.source.channels, 1);
        assert_eq!(out.pixels, [17, 17, 17, 255, 230, 230, 230, 255]);
    }

    /// Build an uncompressed true-colour TGA (image type 2) by hand — the variant every art
    /// tool writes, and the one no encoder in our test deps produces (`image` writes RLE).
    fn raw_tga(width: u16, height: u16, bpp: u8, pixels: &[u8]) -> Vec<u8> {
        let mut v = vec![
            0, // id length
            0, // colour-map type: none
            2, // image type: uncompressed true-colour
            0, 0, // colour-map origin
            0, 0, // colour-map length
            0, // colour-map entry size
            0, 0, // x origin
            0, 0, // y origin
        ];
        v.extend_from_slice(&width.to_le_bytes());
        v.extend_from_slice(&height.to_le_bytes());
        v.push(bpp);
        v.push(0x20); // descriptor: top-left origin, no attribute bits
        v.extend_from_slice(pixels);
        v.extend_from_slice(b"\0\0\0\0\0\0\0\0TRUEVISION-XFILE.\0");
        v
    }

    /// An uncompressed true-colour TGA opens as a TGA and not as a cursor.
    ///
    /// Its first four bytes are `00 00 02 00` — id length 0, no colour map, image type 2, and the
    /// low byte of the colour-map origin — which is byte-for-byte the Windows `.cur` magic (`00 00`
    /// reserved, type word `2`). `sniff` claimed that magic for CUR before anything could consider
    /// TGA, so the most ordinary TGA there is (what Photoshop, Substance and every game-texture
    /// pipeline writes) went to the ICO decoder and failed to open at all, extension hint or not.
    #[test]
    fn uncompressed_truecolor_tga_is_not_a_cursor() {
        // BGR, top-left origin.
        let bytes = raw_tga(2, 1, 24, &[40, 30, 200, 60, 220, 10]);
        assert_eq!(
            &bytes[..4],
            &[0x00, 0x00, 0x02, 0x00],
            "this fixture only tests the collision if it actually collides"
        );

        for ext in [Some("tga"), Some("TGA"), None] {
            let out = decode(&bytes, ext, &DecodeOptions::viewer())
                .unwrap_or_else(|e| panic!("ext {ext:?}: {e:?}"));
            assert_eq!(out.source.name, "TGA");
            assert_eq!((out.width, out.height), (2, 1));
            assert_eq!(out.source.channels, 3);
            assert_eq!(&out.pixels[0..4], &[200, 30, 40, 255]);
        }

        // 32-bit, the other common uncompressed variant, collides identically.
        let bytes = raw_tga(1, 1, 32, &[3, 2, 1, 128]);
        let out = decode(&bytes, Some("tga"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.source.name, "TGA");
        assert_eq!(&out.pixels[0..4], &[1, 2, 3, 128]);
    }

    /// Encode a GIF from a list of `(solid RGBA color, delay ms)` frames (test fixture). One frame
    /// → a still GIF; more → animated.
    fn encode_gif(w: u32, h: u32, frames: &[([u8; 4], u32)]) -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame, RgbaImage};
        let mut buf = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut buf);
            enc.set_repeat(Repeat::Infinite).expect("set repeat");
            for (color, delay_ms) in frames {
                let img = RgbaImage::from_pixel(w, h, image::Rgba(*color));
                let frame = Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(*delay_ms, 1));
                enc.encode_frame(frame).expect("encode gif frame");
            }
        }
        buf
    }

    /// An animated GIF decodes every frame, carrying each frame's delay, with frame 0 also in
    /// `pixels` (the still path). Routed by the `GIF8` magic to the multi-frame decoder.
    #[test]
    fn animated_gif_decodes_all_frames_with_delays() {
        let bytes = encode_gif(4, 4, &[([220, 30, 40, 255], 100), ([20, 60, 220, 255], 60)]);
        let out = decode(&bytes, Some("gif"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.source.name, "GIF");
        assert_eq!((out.width, out.height), (4, 4));
        assert_eq!(out.format, PixelFormat::Rgba8Unorm);
        assert_eq!(out.source.channels, 4);

        let anim = out
            .animation
            .as_ref()
            .expect("animated GIF carries an Animation");
        assert_eq!(anim.frames.len(), 2);
        // Delays round-trip (GIF stores centiseconds; both are multiples of 10 ms, ≥ 20 ms).
        assert_eq!(anim.frames[0].delay_ms, 100);
        assert_eq!(anim.frames[1].delay_ms, 60);
        // Frame 0's pixels are duplicated into `pixels` so the still-image path works unchanged.
        assert_eq!(out.pixels, anim.frames[0].pixels);
        // Solid colors survive GIF palette quantization: frame 0 red-ish, frame 1 blue-ish.
        let f0 = &anim.frames[0].pixels;
        assert!(
            f0[0] > 180 && f0[1] < 90 && f0[2] < 100,
            "frame0 {},{},{}",
            f0[0],
            f0[1],
            f0[2]
        );
        let f1 = &anim.frames[1].pixels;
        assert!(
            f1[2] > 180 && f1[0] < 90,
            "frame1 {},{},{}",
            f1[0],
            f1[1],
            f1[2]
        );
    }

    /// GIF delays of 0 (and other sub-20 ms values) are clamped to 100 ms, matching how browsers
    /// treat "as fast as possible" — so a 0-delay GIF plays at a sane speed instead of spinning.
    #[test]
    fn gif_zero_delay_clamped_to_100ms() {
        let bytes = encode_gif(2, 2, &[([1, 2, 3, 255], 0), ([9, 8, 7, 255], 0)]);
        let out = decode(&bytes, Some("gif"), &DecodeOptions::viewer()).unwrap();
        let anim = out.animation.as_ref().expect("animated");
        assert!(
            anim.frames.iter().all(|f| f.delay_ms == 100),
            "0-delay frames clamp to 100 ms"
        );
    }

    /// A single-frame GIF is an ordinary still image — no `Animation`, so no playback timer.
    #[test]
    fn single_frame_gif_is_still() {
        let bytes = encode_gif(2, 2, &[([10, 200, 60, 255], 100)]);
        let out = decode(&bytes, Some("gif"), &DecodeOptions::viewer()).unwrap();
        assert_eq!(out.source.name, "GIF");
        assert_eq!((out.width, out.height), (2, 2));
        assert!(
            out.animation.is_none(),
            "a single-frame GIF is a still image"
        );
    }

    /// FD7: a panic inside a decode comes back as an error, and the thread is fine afterwards.
    #[test]
    fn a_panic_is_an_error() {
        let r: Result<(), DecodeError> = guarded(|| panic!("a decoder bug"));
        assert!(matches!(r, Err(DecodeError::Panicked)));
        assert!(guarded(|| Ok(1)).is_ok());
    }

    /// A panic hook can tell a decode's panic from any other, on the decode's own thread and on
    /// the helpers its passes run on.
    #[test]
    fn in_decode_covers_the_decode_and_its_helpers() {
        assert!(!in_decode());
        let seen = std::sync::atomic::AtomicBool::new(true);
        guarded(|| {
            assert!(in_decode());
            let mut buf = vec![0u8; 16 << 20];
            par_chunks_mut(&mut buf, 4, |_, _| {
                if !in_decode() {
                    seen.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            });
            Ok(())
        })
        .unwrap();
        assert!(seen.into_inner());
        assert!(!in_decode());
    }

    /// Corrupt input must surface an error, never panic (FFI-free path, but the viewer
    /// relies on this being a clean `Err`).
    #[test]
    fn garbage_input_errors() {
        let bytes = b"\x89PNG\r\n\x1a\n garbage that is not a real png body";
        let r = decode(bytes, Some("png"), &DecodeOptions::viewer());
        assert!(r.is_err());
    }

    // --- The one extension table --------------------------------------------------------------

    /// Every raw format the decoder *routes* (`raw::EXT_LABELS`) must also be a format the app
    /// admits it can open. Miss one and the file decodes fine when opened directly but is
    /// invisible to the Open dialog and skipped by folder navigation.
    #[test]
    fn raw_extensions_are_all_listed() {
        for (ext, label) in raw::EXT_LABELS {
            assert!(
                all_extensions().contains(ext),
                "raw.rs routes .{ext} ({label}) but all_extensions() omits it"
            );
        }
    }

    /// The table is a set, not a bag — a duplicate would be harmless but signals an edit collision.
    #[test]
    fn extension_table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for ext in all_extensions() {
            assert!(seen.insert(*ext), ".{ext} is listed twice");
            assert_eq!(
                *ext,
                ext.to_ascii_lowercase(),
                "extensions are stored lower-case"
            );
        }
    }

    /// Routing is a *measured* decision (§6): PNG and HDR are sniffed here but decoded by the
    /// `image` crate, whose decoders beat zune's for both. Nothing else in this file can see
    /// that — every other test asserts decoded pixels, and those come out identical whichever
    /// backend produced them, so a format quietly demoted to the generic fallback would leave
    /// the suite green and only show up as a slower decode.
    ///
    /// Worth pinning because the sniff is not ours: `sniff` asks zune's `guess_format`, whose
    /// coverage moves with zune's own feature flags (its BMP and WebP probes are `#[cfg]`-gated;
    /// its magic table currently is not). We enable only the formats we route to zune, so that
    /// boundary is exactly where a dependency bump or a feature edit can shift routing without
    /// touching a line of this crate. Assert the backend, not the pixels.
    #[test]
    fn png_and_hdr_route_to_their_own_backends_not_the_fallback() {
        let png = encode(
            &image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2)),
            image::ImageFormat::Png,
        );
        assert!(
            matches!(route_of(&png, None), Backend::Png),
            "PNG must reach decode_png, not the `image` fallback"
        );

        // Both Radiance signatures, as whole little files: zune's sniffer peeks a fixed window,
        // so a fixture trimmed to the bare magic would fail for reasons a real file never hits.
        for magic in ["#?RADIANCE", "#?RGBE"] {
            let mut hdr = format!("{magic}\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n").into_bytes();
            hdr.extend_from_slice(&[128, 128, 128, 129]);
            assert!(
                matches!(route_of(&hdr, None), Backend::Hdr),
                "Radiance HDR ({magic}) must reach decode_hdr, not the `image` fallback"
            );
        }

        // And the formats zune *does* own still go to zune.
        let jpg = encode(
            &image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8)),
            image::ImageFormat::Jpeg,
        );
        assert!(matches!(route_of(&jpg, None), Backend::Zune(_)));
    }

    // --- Decode-bomb guards -------------------------------------------------------------------
    //
    // Each of these is a *tiny* file whose header declares an enormous image. They must come back
    // as a clean `Err` from the header check, never reaching an allocation: a `Vec` that fails to
    // allocate aborts the process (`handle_alloc_error`), which no `catch_unwind` can intercept —
    // so "the test passes" and "the test process is still alive" are the same assertion here.

    /// The zune and image-crate label tables overlap, and the zune→image fallback means the
    /// *same file* can be named by either depending on which decoder won — so the shared
    /// entries must agree, or the status bar's format label changes spelling with the decode
    /// path taken.
    #[test]
    fn format_label_tables_agree_on_shared_formats() {
        use zune_image::codecs::ImageFormat as Z;
        assert_eq!(
            zune_format_name(Z::JPEG),
            format_name(image::ImageFormat::Jpeg)
        );
        assert_eq!(
            zune_format_name(Z::BMP),
            format_name(image::ImageFormat::Bmp)
        );
        assert_eq!(
            zune_format_name(Z::WEBP),
            format_name(image::ImageFormat::WebP)
        );
    }

    /// The product, not the axes, is what gets allocated: both of these pass a per-axis cap of
    /// 131072 and still ask for far more than [`Limits::VIEWER.max_bytes`].
    #[test]
    fn check_dims_bounds_the_product_not_just_each_axis() {
        // Comfortably inside the per-axis cap; 65535² × 4 ≈ 17 GiB.
        assert!(check_dims(65535, 65535, 4, "GIF", &Limits::VIEWER).is_err());
        // A single oversized axis is still refused.
        assert!(
            check_dims(
                Limits::VIEWER.max_dim as usize + 1,
                1,
                4,
                "PNG",
                &Limits::VIEWER
            )
            .is_err()
        );
        // Overflowing the multiply is a rejection, not a wrap.
        assert!(check_dims(usize::MAX, usize::MAX, 16, "OpenEXR", &Limits::VIEWER).is_err());
        // A large-but-real image still decodes: a 216-MP 16-bit scan is ~1.7 GiB.
        assert!(check_dims(18000, 12000, 8, "PNG", &Limits::VIEWER).is_ok());
    }

    /// A 33-byte PNG whose IHDR claims 2³¹-ish pixels per side. The contract under test is
    /// behavioral, not which layer enforces it: a decode bomb comes back as a clean `Err` and the
    /// process survives. (Here the `png` crate's own memory limit happens to refuse it first;
    /// [`check_dims`] is the backstop for the sizes that slip under that.)
    #[test]
    fn png_decode_bomb_header_is_rejected() {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&0x7fff_ffffu32.to_be_bytes()); // width
        ihdr.extend_from_slice(&0x7fff_ffffu32.to_be_bytes()); // height
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, deflate, no filter/interlace

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes.extend_from_slice(&(ihdr.len() as u32 - 4).to_be_bytes());
        bytes.extend_from_slice(&ihdr);
        bytes.extend_from_slice(&crc32(&ihdr).to_be_bytes());

        assert!(
            decode(&bytes, Some("png"), &DecodeOptions::viewer()).is_err(),
            "a 2³¹×2³¹ IHDR must be refused, not allocated"
        );
    }

    /// Radiance stores its dimensions as decimal text, so a 52-byte file can claim a gigapixel
    /// canvas — 16 bytes/px once expanded to float RGBA, i.e. ~160 GiB.
    #[test]
    fn hdr_decode_bomb_header_is_rejected() {
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 99999 +X 99999\n";
        let err = decode(bytes, Some("hdr"), &DecodeOptions::viewer())
            .expect_err("a 99999² Radiance header must be refused");
        assert!(err.to_string().contains("decode guard"), "{err}");
    }

    /// GIF is the case a per-axis cap *cannot* catch, and the reason [`check_dims`] takes a byte
    /// budget: `u16` dimensions max out at 65535, comfortably under any sane axis guard, yet
    /// 65535² × 4 bytes is ~17 GiB — per frame. A complete but tiny file (one 1×1 frame on a
    /// 65535² logical screen) is all it takes; the canvas, not the frame, is what gets allocated.
    #[test]
    fn gif_max_u16_canvas_is_rejected_by_the_byte_budget() {
        #[rustfmt::skip]
        let bytes: Vec<u8> = [
            b"GIF89a".as_slice(),
            &[0xff, 0xff],              // logical screen width  = 65535
            &[0xff, 0xff],              // logical screen height = 65535
            &[0x80, 0x00, 0x00],        // global color table (2 entries), bg index, aspect
            &[0x00, 0x00, 0x00],        // GCT[0] = black
            &[0xff, 0xff, 0xff],        // GCT[1] = white
            &[0x2c],                    // image separator
            &[0x00, 0x00, 0x00, 0x00],  // frame left, top
            &[0x01, 0x00, 0x01, 0x00],  // frame width = 1, height = 1
            &[0x00],                    // no local color table
            &[0x02],                    // LZW minimum code size
            &[0x02, 0x44, 0x01],        // one sub-block: CLEAR, index 0, EOI
            &[0x00],                    // block terminator
            &[0x3b],                    // trailer
        ]
        .concat();

        let err = decode(&bytes, Some("gif"), &DecodeOptions::viewer())
            .expect_err("a 65535² GIF canvas must be refused");
        assert!(err.to_string().contains("decode guard"), "{err}");
    }

    /// An animated GIF's cost is `frames × w × h × 4`, so the frame count is bounded too — a small
    /// canvas must not let an unbounded sequence through. 100 frames of 4×4 is far under the cap
    /// and decodes whole; the cap itself is exercised by [`MAX_ANIMATION_FRAMES`] arithmetic.
    #[test]
    fn animation_frame_budget_admits_real_sequences() {
        let frames: Vec<_> = (0..100u32)
            .map(|i| ([(i * 2) as u8, 40, 200, 255], 40u32))
            .collect();
        let bytes = encode_gif(4, 4, &frames);
        let out = decode(&bytes, Some("gif"), &DecodeOptions::viewer()).unwrap();
        let anim = out.animation.as_ref().expect("animated");
        assert_eq!(
            anim.frames.len(),
            100,
            "a 100-frame 4×4 GIF is nowhere near the budget"
        );

        // The budget divides the byte cap by the canvas size, and is clamped to the frame ceiling.
        let tiny_canvas_budget =
            ((MAX_ANIMATION_BYTES / (4 * 4 * 4)) as usize).min(MAX_ANIMATION_FRAMES);
        assert_eq!(tiny_canvas_budget, MAX_ANIMATION_FRAMES);
    }

    /// The zune hot path is guarded by the *product*, not just each axis — the case its own
    /// `set_max_width`/`set_max_height` options cannot express.
    ///
    /// A real 8×8 JPEG with its SOF dimensions overwritten to 65535×65535: a ~300-byte file that
    /// declares 4.3 gigapixels, i.e. ~17 GiB of RGBA. Both axes are under the 131072 per-axis cap,
    /// so zune would have accepted it and allocated — and an allocation that fails aborts the
    /// process, which no `catch_unwind` can catch.
    #[test]
    fn zune_jpeg_decode_bomb_is_rejected_by_the_byte_budget() {
        let mut jpg = encode(
            &image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8)),
            image::ImageFormat::Jpeg,
        );

        // Find SOF0 (FF C0) and overwrite its 16-bit height and width fields.
        // Segment layout: FF, C0, len(2), precision(1), height(2), width(2).
        let sof = jpg
            .windows(2)
            .position(|w| w == [0xFF, 0xC0])
            .expect("baseline JPEG has an SOF0");
        jpg[sof + 5..sof + 7].copy_from_slice(&u16::MAX.to_be_bytes()); // height
        jpg[sof + 7..sof + 9].copy_from_slice(&u16::MAX.to_be_bytes()); // width
        assert!(
            jpg.len() < 1000,
            "the bomb is a tiny file: {} bytes",
            jpg.len()
        );

        let err = decode(&jpg, Some("jpg"), &DecodeOptions::viewer())
            .expect_err("a 65535x65535 JPEG must be refused before zune allocates");
        assert!(err.to_string().contains("decode guard"), "{err}");
    }

    /// Same shape, different container: BMP declares its dimensions as two `i32`s in the DIB
    /// header, so the bomb is two field writes.
    #[test]
    fn zune_bmp_decode_bomb_is_rejected_by_the_byte_budget() {
        let mut bmp = encode(
            &image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4)),
            image::ImageFormat::Bmp,
        );
        // BITMAPINFOHEADER: width at byte 18, height at byte 22 (little-endian i32).
        bmp[18..22].copy_from_slice(&40_000i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&40_000i32.to_le_bytes());

        let err = decode(&bmp, Some("bmp"), &DecodeOptions::viewer())
            .expect_err("a 40000x40000 BMP (6.4 GiB of RGBA) must be refused");
        assert!(err.to_string().contains("decode guard"), "{err}");
    }

    /// The zune guard reads its dimensions from `read_headers`, whose trait default is
    /// `Ok(None)` — i.e. "no metadata", which [`check_zune_dims`] treats as "cannot check".
    /// If a zune upgrade ever dropped that impl for a format we route, the guard would quietly
    /// become a no-op and every test above would still pass. So assert the probe actually sees a
    /// size for each format on the hot path.
    #[test]
    fn zune_read_headers_is_not_vacuous() {
        use zune_core::bytestream::ZCursor;
        use zune_core::options::DecoderOptions;
        use zune_image::codecs::ImageFormat;

        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(7, 5));
        for (fmt, bytes) in [
            (ImageFormat::JPEG, encode(&img, image::ImageFormat::Jpeg)),
            (ImageFormat::BMP, encode(&img, image::ImageFormat::Bmp)),
        ] {
            let mut dec = fmt
                .decoder_with_options(ZCursor::new(&bytes), DecoderOptions::new_fast())
                .expect("decoder");
            let md = dec
                .read_headers()
                .expect("headers parse")
                .unwrap_or_else(|| panic!("{fmt:?} reports no metadata — the size guard is blind"));
            assert_eq!(
                md.dimensions(),
                (7, 5),
                "{fmt:?} header dimensions feed check_dims; they must be the real ones"
            );
        }
    }

    /// CRC-32 (IEEE) for building the PNG fixture above.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }
}
