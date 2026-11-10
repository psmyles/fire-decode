//! The formats the crate recognises (FD9): [`Format`], [`sniff`](crate::sniff)'s answer, and the two
//! extension lists apps build their file filters from.

/// An image format the crate recognises, whatever this build decodes. [`sniff`](crate::sniff)
/// returns one, [`probe`](crate::probe) and [`SourceInfo`](crate::SourceInfo) carry one, and
/// [`DecodeError::Unsupported`](crate::DecodeError::Unsupported) names one compiled out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tiff,
    Tga,
    Ico,
    /// A Windows cursor: an ICO container with a different type word.
    Cur,
    WebP,
    Qoi,
    /// PPM, PGM, PBM, PAM and PFM.
    Pnm,
    Farbfeld,
    JpegXl,
    RadianceHdr,
    Exr,
    Dds,
    /// PSD, and PSB by its extension. The crate reads PSD only: a PSB fails to decode, but is
    /// listed because fire-cli checks one from its header.
    Psd,
    Heic,
    Heif,
    Avif,
    /// A camera raw, decoded from its embedded JPEG preview.
    CameraRaw,
    /// Nothing the crate recognises.
    Unknown,
}

impl Format {
    /// The name people use for it: `PNG`, `JPEG`, `OpenEXR`.
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
            Format::Tiff => "TIFF",
            Format::Tga => "TGA",
            Format::Ico => "ICO",
            Format::Cur => "CUR",
            Format::WebP => "WebP",
            Format::Qoi => "QOI",
            Format::Pnm => "PNM",
            Format::Farbfeld => "Farbfeld",
            Format::JpegXl => "JPEG XL",
            Format::RadianceHdr => "Radiance HDR",
            Format::Exr => "OpenEXR",
            Format::Dds => "DDS",
            Format::Psd => "PSD",
            Format::Heic => "HEIC",
            Format::Heif => "HEIF",
            Format::Avif => "AVIF",
            Format::CameraRaw => "Camera raw",
            Format::Unknown => "unknown",
        }
    }

    /// Whether this build decodes it. PSD needs the `psd` feature, HEIC / HEIF / AVIF `heif`, and
    /// camera raw `raw`; the rest are always in. `Unknown` is not.
    // The arms are `cfg!`s, so in a build without features they all fold to `false` and clippy
    // reads the match as a `matches!`; it is a table per feature.
    #[allow(clippy::match_like_matches_macro)]
    pub fn is_supported(self) -> bool {
        match self {
            Format::Psd => cfg!(feature = "psd"),
            Format::Heic | Format::Heif | Format::Avif => cfg!(feature = "heif"),
            Format::CameraRaw => cfg!(feature = "raw"),
            Format::Unknown => false,
            _ => true,
        }
    }

    /// The cargo feature that brings it in, for those that need one.
    pub(crate) fn feature(self) -> Option<&'static str> {
        match self {
            Format::Psd => Some("psd"),
            Format::Heic | Format::Heif | Format::Avif => Some("heif"),
            Format::CameraRaw => Some("raw"),
            _ => None,
        }
    }

    /// The format a file extension names (no leading dot, any case), if any.
    pub fn from_extension(ext: &str) -> Option<Format> {
        let lower = ext.to_ascii_lowercase();
        let ext = lower.as_str();
        Some(match ext {
            "png" => Format::Png,
            "jpg" | "jpeg" | "jpe" | "jfif" => Format::Jpeg,
            "gif" => Format::Gif,
            "bmp" | "dib" => Format::Bmp,
            "tif" | "tiff" | "tx" => Format::Tiff,
            "webp" => Format::WebP,
            "ico" => Format::Ico,
            "cur" => Format::Cur,
            "tga" => Format::Tga,
            "qoi" => Format::Qoi,
            "ppm" | "pgm" | "pbm" | "pnm" | "pam" => Format::Pnm,
            "ff" => Format::Farbfeld,
            "jxl" => Format::JpegXl,
            "hdr" | "pic" | "rgbe" | "xyze" => Format::RadianceHdr,
            "exr" => Format::Exr,
            "dds" => Format::Dds,
            "psd" | "psb" => Format::Psd,
            "heic" => Format::Heic,
            "heif" => Format::Heif,
            "avif" => Format::Avif,
            _ if crate::raw::EXT_LABELS.iter().any(|(e, _)| *e == ext) => Format::CameraRaw,
            _ => return None,
        })
    }
}

/// Every extension the crate recognises, lower-case, whatever this build decodes. For tests that
/// run in CI's no-features job and must still see the whole set: fire's installer test and
/// fire-thumbs' format-table test.
///
/// A convenience for *naming* files, not the routing decision: [`decode`](crate::decode) sniffs
/// magic bytes and opens a supported image with the wrong extension, or none.
pub fn all_extensions() -> &'static [&'static str] {
    ALL_EXTENSIONS
}

/// The extensions of the formats this build decodes ([`Format::is_supported`]): no `.psd` without
/// `psd`, no `.heic` without `heif`, no `.nef` without `raw`. What an app's Open dialog, folder
/// navigation and drag-and-drop accept.
pub fn supported_extensions() -> &'static [&'static str] {
    static SUPPORTED: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        ALL_EXTENSIONS
            .iter()
            .copied()
            .filter(|e| Format::from_extension(e).is_some_and(Format::is_supported))
            .collect()
    });
    &SUPPORTED
}

/// Whether `ext` (no leading dot, any case) is in [`supported_extensions`].
pub fn is_supported_extension(ext: &str) -> bool {
    Format::from_extension(ext).is_some_and(Format::is_supported)
}

const ALL_EXTENSIONS: &[&str] = &[
    // Still formats, in the order the crate's doc lists the backends.
    "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "dib", "tif", "tiff", "tx", "webp", "ico",
    "cur", "tga", "qoi", "ppm", "pgm", "pbm", "pnm", "pam", "ff", "jxl", "hdr", "pic", "rgbe",
    "xyze", "exr", "dds", "psd", "psb", "heic", "heif", "avif", //
    // Camera raw (embedded-preview decode). Mirrors `raw::EXT_LABELS`, which is what routes them;
    // `raw_extensions_are_all_listed` keeps the two honest.
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw",
    "dng", "x3f", "3fr", "fff", "iiq", "erf", "mrw", "dcr", "kdc", "mef", "mos", "rwl", "gpr",
    "raw",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_extension_names_a_format() {
        for ext in all_extensions() {
            assert!(
                Format::from_extension(ext).is_some(),
                ".{ext} names nothing"
            );
            assert!(
                Format::from_extension(&ext.to_ascii_uppercase()).is_some(),
                ".{ext} is case-sensitive"
            );
        }
    }

    #[test]
    fn supported_extensions_follow_the_features() {
        let has = |e: &str| supported_extensions().contains(&e);
        assert_eq!(has("psd"), cfg!(feature = "psd"));
        assert_eq!(has("psb"), cfg!(feature = "psd"));
        assert_eq!(has("heic"), cfg!(feature = "heif"));
        assert_eq!(has("avif"), cfg!(feature = "heif"));
        assert_eq!(has("nef"), cfg!(feature = "raw"));
        assert_eq!(has("dng"), cfg!(feature = "raw"));
        for e in ["png", "jpg", "exr", "dds", "tga", "hdr", "jxl"] {
            assert!(has(e), ".{e} is always in");
        }
        for e in supported_extensions() {
            assert!(all_extensions().contains(e));
            assert!(is_supported_extension(e));
            assert!(is_supported_extension(&e.to_ascii_uppercase()));
        }
        assert!(!is_supported_extension("txt"));
    }
}
