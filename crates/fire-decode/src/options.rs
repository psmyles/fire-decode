//! How a decode is asked for: [`DecodeOptions`] and its named setups (FD5), the [`Limits`] each
//! setup carries, and the [`DecodeCtl`] a caller watches or stops a decode through.

/// What a decode may allocate, per setup (FD5). A header past either is refused with
/// [`DecodeError::TooLarge`](crate::DecodeError::TooLarge) before anything is allocated from it.
///
/// The same values go to libheif's security limits (FD16) and to the PSD bridge (FD3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest source dimension on either axis, in pixels.
    pub max_dim: u32,
    /// Largest decoded buffer, in bytes: width × height × the bytes per pixel of the RGBA buffer
    /// the source decodes to. The product is what gets allocated; a per-axis cap alone bounds
    /// nothing useful (a 65535×65535 GIF is under every axis cap and asks for 17 GiB).
    pub max_bytes: u64,
}

impl Limits {
    /// fire's viewer and fire-cli: one image at a time, with headroom for a 216-MP scan
    /// (~1.7 GiB at 16-bit). 131072 px, 4 GiB.
    pub const VIEWER: Limits = Limits {
        max_dim: 1 << 17,
        max_bytes: 4 << 30,
    };
    /// 3D Review's textures and bite's thumbnails: many decodes at once. 30000 px, 1 GiB, the
    /// values 3D Review chose for its PSD path.
    pub const TEXTURE: Limits = Limits {
        max_dim: 30_000,
        max_bytes: 1 << 30,
    };
}

/// The buffer a decode returns (FD4). The file itself is described by
/// [`DecodedImage::source`](crate::DecodedImage::source), whatever this narrows to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// As the source holds it: 8 or 16-bit unorm, half or 32-bit float RGBA.
    Native,
    /// Always `Rgba8Unorm`. 16-bit rounds to nearest; float clamps to 0..1 with no transfer curve.
    /// Runs after any [`fit`](DecodeOptions::fit), so a thumbnail narrows 256 px, not 8K.
    Rgba8,
    /// Unorm sources as [`Rgba8`](Self::Rgba8); float sources stay float, as
    /// [`Native`](Self::Native) returns them. 3D Review narrows float per material slot itself.
    Rgba8OrFloat,
}

/// How HDR sources come back (FD17). It changes only HDR sources: a PQ or HLG HEIF / AVIF, and
/// (from step 6c) a photo with a gain map. Every other file decodes the same either way.
/// [`probe`](crate::probe) tells which kind a file is before it is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hdr {
    /// An SDR picture. A PQ / HLG source is decoded to light, put at 203 cd/m² (BT.2408's
    /// reference white) = white, clipped there, converted to Rec.709 and sRGB-encoded, into
    /// `Rgba16Unorm` for [`output`](DecodeOptions::output) to narrow. No tone curve: the highlights
    /// above reference white are what an SDR rendering gives up. A gain-map photo is its SDR base.
    Sdr,
    /// HDR as linear light: a PQ / HLG source comes back as `Rgba16Float`, Rec.709 primaries,
    /// 203 cd/m² = 1.0, colours outside Rec.709 kept, negatives included (a viewer's tone curve
    /// must clamp them). PQ's 10,000 cd/m² peak is about 49. Needs an output that keeps float:
    /// with [`Output::Rgba8`] the decode is refused.
    Linear,
}

/// The filter a [`Fit`] shrinks with (FD6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Nearest neighbour: fire's RAM guard for images past 16K, where bounding memory matters more
    /// than filter quality.
    Nearest,
    /// An area average of the stored values (not linear light), alpha-weighted so transparent
    /// pixels lend no colour: bite's thumbnails.
    Area,
}

/// Shrink to fit within `max` on both axes, keeping the aspect ratio. Never enlarges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub max: u32,
    pub filter: Filter,
}

/// Which frames of an animation come back (FD5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frames {
    /// Every frame of an animated GIF, in [`Animation`](crate::Animation). HEIF sequences join in
    /// step 6d.
    All,
    /// The first frame only, as a still.
    First,
}

/// What a DDS file's array layers, cube faces or volume slices come back as (FD5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surfaces {
    /// All of them, tiled into one sheet described by
    /// [`DecodedImage::layout`](crate::DecodedImage::layout).
    Sheet,
    /// The first surface only: layer 0, face +X or slice 0.
    First,
}

/// Options for one decode. There is no `Default`: each app starts from the named setup that says
/// what it wants (FD5), so a new field forces a decision per setup instead of inheriting fire's.
/// Every field is public for one-off adjustments, e.g. fire-shellext's
/// `DecodeOptions { frames: Frames::First, stored_mips: false, ..DecodeOptions::viewer() }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodeOptions {
    /// Convert an embedded ICC profile, or a wide-gamut nclx colour description with no profile,
    /// to sRGB. The profile bytes come back in `icc` either way.
    #[cfg(feature = "icc")]
    pub honor_icc: bool,
    /// Rotate by the EXIF `Orientation` tag (and a camera raw's preview orientation). HEIF's own
    /// `irot` / `imir` / `clap` and JPEG XL's header orientation are part of the image, and are
    /// always applied.
    pub honor_exif: bool,
    pub output: Output,
    pub hdr: Hdr,
    pub fit: Option<Fit>,
    pub frames: Frames,
    pub surfaces: Surfaces,
    /// Keep a DDS file's own mip chain in
    /// [`DecodedImage::source_mips`](crate::DecodedImage::source_mips).
    pub stored_mips: bool,
    pub limits: Limits,
    /// Cap this decode's threads, the calling thread included (benchmarks, determinism). `None`
    /// draws on the process-wide budget alone (FD8).
    pub max_threads: Option<usize>,
}

impl DecodeOptions {
    /// fire's viewer: colour-managed and upright, every animation frame and DDS surface, the
    /// file's own mips, shrunk only past 16384 px (a RAM guard).
    pub fn viewer() -> Self {
        Self {
            #[cfg(feature = "icc")]
            honor_icc: true,
            honor_exif: true,
            output: Output::Native,
            hdr: Hdr::Sdr,
            fit: Some(Fit {
                max: 16384,
                filter: Filter::Nearest,
            }),
            frames: Frames::All,
            surfaces: Surfaces::Sheet,
            stored_mips: true,
            limits: Limits::VIEWER,
            max_threads: None,
        }
    }

    /// fire-cli (Asset Check): the file as stored, at full size, with no colour conversion and no
    /// EXIF rotation, so it reports the size Unity imports. A DDS file's whole sheet, so its
    /// surfaces can be counted from `layout`.
    pub fn inspect() -> Self {
        Self {
            #[cfg(feature = "icc")]
            honor_icc: false,
            honor_exif: false,
            output: Output::Native,
            hdr: Hdr::Sdr,
            fit: None,
            frames: Frames::First,
            surfaces: Surfaces::Sheet,
            stored_mips: false,
            limits: Limits::VIEWER,
            max_threads: None,
        }
    }

    /// 3D Review's textures: as stored, unrotated (a rotated texture no longer matches its UVs),
    /// 8-bit unless the source is float, and never shrunk; 3D Review uploads an oversize texture
    /// from the first mip of its own chain that fits.
    pub fn texture() -> Self {
        Self {
            #[cfg(feature = "icc")]
            honor_icc: false,
            honor_exif: false,
            output: Output::Rgba8OrFloat,
            hdr: Hdr::Sdr,
            fit: None,
            frames: Frames::First,
            surfaces: Surfaces::First,
            stored_mips: false,
            limits: Limits::TEXTURE,
            max_threads: None,
        }
    }

    /// bite's thumbnails: 8-bit, area-averaged to fit `size`, as stored (which is what keeps them
    /// matching ImageMagick's `-thumbnail`, which neither converts the profile nor rotates).
    pub fn thumbnail(size: u32) -> Self {
        Self {
            #[cfg(feature = "icc")]
            honor_icc: false,
            honor_exif: false,
            output: Output::Rgba8,
            hdr: Hdr::Sdr,
            fit: Some(Fit {
                max: size,
                filter: Filter::Area,
            }),
            frames: Frames::First,
            surfaces: Surfaces::First,
            stored_mips: false,
            limits: Limits::TEXTURE,
            max_threads: None,
        }
    }
}

/// How a caller follows and stops a decode (FD16), for
/// [`decode_with_progress`](crate::decode_with_progress).
pub struct DecodeCtl<'a> {
    /// A fraction in `0.0..=1.0`, only ever rising. May be called from the decoder's own threads.
    /// Only the backends that can say call it: OpenEXR per chunk, Radiance HDR per scanline read,
    /// and HEIF per grid tile (a single-tile HEIF reports just its start and end).
    pub progress: &'a (dyn Fn(f32) + Sync),
    /// Polled between the decode's stages, and by libheif while it decodes; `true` abandons the
    /// decode with [`DecodeError::Cancelled`](crate::DecodeError::Cancelled).
    pub cancel: &'a (dyn Fn() -> bool + Sync),
}

impl std::fmt::Debug for DecodeCtl<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodeCtl").finish_non_exhaustive()
    }
}

fn no_progress(_: f32) {}

fn never() -> bool {
    false
}

impl DecodeCtl<'static> {
    /// No progress reports, never cancelled: what [`decode`](crate::decode) uses.
    pub const NONE: DecodeCtl<'static> = DecodeCtl {
        progress: &no_progress,
        cancel: &never,
    };
}

/// The caller's progress callback, held to only ever rising however many threads report into it.
pub(crate) struct Progress<'a> {
    user: &'a (dyn Fn(f32) + Sync),
    last: std::sync::Mutex<f32>,
}

impl<'a> Progress<'a> {
    pub(crate) fn new(user: &'a (dyn Fn(f32) + Sync)) -> Self {
        Self {
            user,
            last: std::sync::Mutex::new(0.0),
        }
    }

    /// Pass `p` on if it is above everything passed on so far. The lock is held across the call,
    /// so two threads can't hand the caller their values out of order.
    pub(crate) fn report(&self, p: f32) {
        if !p.is_finite() {
            return;
        }
        let p = p.clamp(0.0, 1.0);
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if p > *last {
            *last = p;
            (self.user)(p);
        }
    }
}
