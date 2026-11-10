//! What the *file* is (FD4), kept apart from the buffer it decoded to: [`SourceInfo`].

use crate::Format;

/// The source file, as opposed to the buffer ([`DecodedImage::format`](crate::DecodedImage::format)):
/// it stays faithful when the crate narrows, rotates or shrinks the pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceInfo {
    pub format: Format,
    /// The name for a status bar, more specific than [`Format::name`] where the file says more:
    /// `Nikon NEF`, a DDS file's pixel format (`DDS BC7 sRGB`), `PPM` for every Netpbm file.
    pub name: &'static str,
    /// The channels the pixels carry: 1 grey, 2 grey + alpha, 3 colour, 4 colour + alpha. A CMYK
    /// JPEG is 3, and a PSD's extra channels beyond its transparency don't count. Reported
    /// faithfully even when an alpha channel is entirely opaque (see
    /// [`alpha_opaque`](crate::DecodedImage::alpha_opaque)).
    pub channels: u8,
    /// Bits per channel: 1, 2 or 4 for low-depth and palette PNGs (a palette image's is the index
    /// width), otherwise 8, 10, 12, 16 or 32.
    pub bit_depth: u8,
    /// Whether the source stores palette indices rather than colours; `channels` is then what the
    /// palette expands to. Only PNG reports it so far.
    pub palette: bool,
    pub sample: Sample,
    /// The colour signalling other than ICC: HEIF's nclx box, or what the format implies (sRGB for
    /// integer samples, linear for float). When the file carries an ICC profile, the profile is
    /// the authority.
    pub transfer: Transfer,
    pub primaries: Primaries,
    pub hdr: HdrKind,
}

impl SourceInfo {
    /// A source with integer samples and no colour signalling of its own: sRGB is presumed.
    /// [`decode`](crate::decode) marks a float buffer's source `Float` / `Linear` afterwards.
    pub(crate) fn new(format: Format, name: &'static str, channels: u8, bit_depth: u8) -> Self {
        Self {
            format,
            name,
            channels,
            bit_depth,
            palette: false,
            sample: Sample::Unorm,
            transfer: Transfer::Srgb,
            primaries: Primaries::Bt709,
            hdr: HdrKind::None,
        }
    }
}

/// How the source stores a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sample {
    /// Integers, read as 0..1.
    Unorm,
    /// Floating point, linear light (OpenEXR, Radiance HDR, float TIFF, 32-bit PSD, PFM).
    Float,
}

/// The source's transfer curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Transfer {
    /// sRGB, said or presumed.
    Srgb,
    Linear,
    /// SMPTE ST 2084, display-referred HDR.
    Pq,
    /// ARIB STD-B67 hybrid log-gamma.
    Hlg,
    /// Another nclx transfer (BT.709's, BT.601's, a plain gamma); SDR, shown as sRGB.
    Other,
}

/// The source's colour primaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Primaries {
    /// BT.709, sRGB's: said or presumed.
    Bt709,
    /// Display P3 (P3 primaries, D65 white).
    DisplayP3,
    Bt2020,
    Other,
}

/// Whether, and how, the source is HDR (FD17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HdrKind {
    None,
    Pq,
    Hlg,
    /// An SDR image plus a gain map (Apple's, or ISO 21496-1). Reported from step 6c.
    GainMap,
}

/// The source description an nclx box gives (ITU-T H.273 code points).
#[cfg(feature = "heif")]
pub(crate) fn from_nclx(transfer: u16, primaries: u16) -> (Transfer, Primaries, HdrKind) {
    let t = match transfer {
        2 | 13 => Transfer::Srgb, // unspecified (presumed sRGB), sRGB
        8 => Transfer::Linear,
        16 => Transfer::Pq,
        18 => Transfer::Hlg,
        _ => Transfer::Other,
    };
    let p = match primaries {
        1 | 2 => Primaries::Bt709, // BT.709, unspecified
        12 => Primaries::DisplayP3,
        9 => Primaries::Bt2020,
        _ => Primaries::Other,
    };
    let hdr = match t {
        Transfer::Pq => HdrKind::Pq,
        Transfer::Hlg => HdrKind::Hlg,
        _ => HdrKind::None,
    };
    (t, p, hdr)
}
