//! Gain maps (fire-decode plan, FD17; step 6c): an SDR photo plus a gain map, rendered as HDR
//! under [`Hdr::Linear`](crate::Hdr::Linear), at the file's full headroom, into the same linear
//! light as PQ / HLG: Rec.709 primaries, the SDR white = 1.0. `Sdr` shows the base, as before.
//!
//! libheif has no gain-map API (re-checked at 1.23.7), so this reads the pieces itself:
//! [`heif_meta`](crate::heif_meta) finds them, libheif decodes the two images, and the
//! metadata is parsed here:
//! - **ISO 21496-1:** the `tmap` item's data. Per channel, the gain-map value `G` (0..1) is
//!   `G^(1/gamma)`, interpolated between `log2` minimum and maximum gains, and applied as
//!   `(base + base offset) * 2^(gain * weight) - alternate offset`. At full headroom the weight
//!   is 1 (0 when the base is the HDR side).
//! - **Apple:** `base * (1 + (headroom - 1) * g)`, `g` the gain-map value through the sRGB curve,
//!   the headroom from the gain map's XMP (`HDRGainMapHeadroom`), or failing that from the
//!   base's MakerNote (tags 33 and 48, Apple's published formula).
//! - **Which:** ISO when both are present, as ImageIO does (every iPhone photo has both, sharing
//!   the gain-map pixels).
//!
//! **Orientation** (checked against ImageIO, `fixtures/heif/README.md`): the base and the gain map
//! are decoded with their transforms ignored. The gain map is stretched over the base's coded
//! size, ignoring its own transforms; Apple's writers store it in the base's coded orientation
//! whatever its `irot` says. Then the **base's** `irot` / `imir` / `clap` are applied, then the
//! `tmap` item's own.
//!
//! **Colour:** the base is decoded through the sRGB curve in its own primaries (nclx, else its ICC
//! profile's colorants, else Rec.709). The gain is applied there, or, when the ISO metadata says
//! the map was made in the alternate image's colour space, in the `tmap` item's nclx primaries.
//! Then everything is converted to Rec.709.

use exr::prelude::f16;

use crate::hdr::{self, Mat};
use crate::heif_meta::{GainMapSource, Meta, Transform};
use crate::{
    Cx, DecodeError, DecodedImage, Format, HdrKind, PixelFormat, Primaries, SourceInfo, budget,
    source,
};

/// The most a `tmap` or XMP item may hold. Real ones are tens and hundreds of bytes.
const MAX_METADATA: usize = 1 << 20;

/// What turns a gain-map value into a multiplier.
#[derive(Debug, Clone, PartialEq)]
enum Params {
    Iso(IsoMetadata),
    /// Apple's headroom, linear (4.0 is two stops).
    Apple(f32),
}

/// ISO 21496-1's metadata, per channel (one channel repeated when the map is single-channel).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IsoMetadata {
    pub(crate) use_base_colour_space: bool,
    /// log2.
    pub(crate) base_headroom: f64,
    /// log2.
    pub(crate) alternate_headroom: f64,
    /// log2.
    pub(crate) min: [f64; 3],
    /// log2.
    pub(crate) max: [f64; 3],
    pub(crate) gamma: [f64; 3],
    pub(crate) base_offset: [f64; 3],
    pub(crate) alternate_offset: [f64; 3],
}

/// Parses a `tmap` item's data. `None` for a version or a minimum version this doesn't read, or a
/// fraction with a zero denominator.
pub(crate) fn parse_iso(b: &[u8]) -> Option<IsoMetadata> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
    };
    if *b.first()? != 0 || u16_at(1)? != 0 {
        return None;
    }
    let flags = *b.get(5)?;
    let channels = if flags & 0x80 != 0 { 3 } else { 1 };
    let mut at = 6;
    let mut frac = |signed: bool| -> Option<f64> {
        let n = u32_at(at)?;
        let d = u32_at(at + 4)?;
        at += 8;
        let n = if signed { n as i32 as f64 } else { n as f64 };
        (d != 0).then(|| n / d as f64)
    };
    let base_headroom = frac(false)?;
    let alternate_headroom = frac(false)?;
    let mut m = IsoMetadata {
        use_base_colour_space: flags & 0x40 != 0,
        base_headroom,
        alternate_headroom,
        min: [0.0; 3],
        max: [0.0; 3],
        gamma: [1.0; 3],
        base_offset: [0.0; 3],
        alternate_offset: [0.0; 3],
    };
    for c in 0..channels {
        m.min[c] = frac(true)?;
        m.max[c] = frac(true)?;
        m.gamma[c] = frac(false)?;
        m.base_offset[c] = frac(true)?;
        m.alternate_offset[c] = frac(true)?;
    }
    if channels == 1 {
        for c in 1..3 {
            m.min[c] = m.min[0];
            m.max[c] = m.max[0];
            m.gamma[c] = m.gamma[0];
            m.base_offset[c] = m.base_offset[0];
            m.alternate_offset[c] = m.alternate_offset[0];
        }
    }
    m.gamma.iter().all(|&g| g > 0.0).then_some(m)
}

/// Apple's headroom from its XMP: `HDRGainMap:HDRGainMapHeadroom`, as an element or an attribute.
pub(crate) fn xmp_headroom(xmp: &[u8]) -> Option<f32> {
    let text = std::str::from_utf8(xmp).ok()?;
    let at = text.find("HDRGainMapHeadroom")? + "HDRGainMapHeadroom".len();
    let rest = text[at..].trim_start_matches(['=', '"', '\'', '>', ' ']);
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .unwrap_or(rest.len());
    rest[..end]
        .parse::<f32>()
        .ok()
        .filter(|h| h.is_finite() && *h >= 1.0)
}

/// Apple's headroom from the base's MakerNote, tags 33 and 48, by Apple's published formula.
/// `exif` is the HEIF `Exif` item: a 4-byte offset to the TIFF header, then EXIF.
pub(crate) fn makernote_headroom(exif: &[u8]) -> Option<f32> {
    use crate::exif::{rd_u16, rd_u32};
    let skip = u32::from_be_bytes(exif.get(..4)?.try_into().ok()?) as usize;
    let tiff = exif.get(4 + skip..)?;
    let le = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    // An IFD's entries: `(tag, type, count, value-or-offset position)`.
    let entries = |b: &[u8], ifd: usize, le: bool| -> Vec<(u16, u16, u32, usize)> {
        let n = rd_u16(b, ifd, le).unwrap_or(0) as usize;
        (0..n.min(512))
            .filter_map(|i| {
                let e = ifd + 2 + i * 12;
                Some((
                    rd_u16(b, e, le)?,
                    rd_u16(b, e + 2, le)?,
                    rd_u32(b, e + 4, le)?,
                    e + 8,
                ))
            })
            .collect()
    };
    let ifd0 = rd_u32(tiff, 4, le)? as usize;
    let exif_ifd = entries(tiff, ifd0, le)
        .into_iter()
        .find(|e| e.0 == 0x8769)
        .and_then(|e| rd_u32(tiff, e.3, le))? as usize;
    let maker = entries(tiff, exif_ifd, le)
        .into_iter()
        .find(|e| e.0 == 0x927c)?;
    let note = tiff
        .get(rd_u32(tiff, maker.3, le)? as usize..)?
        .get(..maker.2 as usize)?;
    // "Apple iOS\0", a version, then "MM" and the IFD at 14; offsets are from the note's start.
    if !note.starts_with(b"Apple iOS\0") {
        return None;
    }
    let mle = note.get(12..14)? == b"II";
    let rational = |tag: u16| -> Option<f64> {
        let e = entries(note, 14, mle).into_iter().find(|e| e.0 == tag)?;
        let at = rd_u32(note, e.3, mle)? as usize;
        let (n, d) = (rd_u32(note, at, mle)?, rd_u32(note, at + 4, mle)?);
        let n = if e.1 == 10 { n as i32 as f64 } else { n as f64 };
        let d = if e.1 == 10 { d as i32 as f64 } else { d as f64 };
        (d != 0.0).then(|| n / d)
    };
    let (m33, m48) = (rational(33)?, rational(48)?);
    let stops = match (m33 < 1.0, m48 <= 0.01) {
        (true, true) => -20.0 * m48 + 1.8,
        (true, false) => -0.101 * m48 + 1.601,
        (false, true) => -70.0 * m48 + 3.0,
        (false, false) => -0.303 * m48 + 2.303,
    };
    Some(2f64.powf(stops.max(0.0)) as f32)
}

/// Renders `base` (the item the file shows) with its gain map as linear HDR. Errors are the
/// caller's to fall back from, except a cancel or a refusal on size, which stand.
pub(crate) fn render(
    bytes: &[u8],
    meta: &Meta<'_>,
    base: u32,
    source_kind: GainMapSource,
    format: Format,
    cx: &Cx,
) -> Result<DecodedImage, DecodeError> {
    let lim = cx.limits();
    let limits = heif_sys::Limits {
        max_dim: lim.max_dim,
        max_output_bytes: lim.max_bytes,
        max_total_memory: lim.max_bytes.saturating_mul(2),
        ..heif_sys::Limits::default()
    };
    let lease = budget::lease(budget::cores());
    let progress = |p: f32| cx.progress(p * 0.8);
    let cancel = || cx.cancelled();
    let ctl = heif_sys::Ctl {
        library_threads: lease.threads(),
        ignore_transformations: true,
        progress: Some(&progress),
        cancel: Some(&cancel),
        ..heif_sys::Ctl::default()
    };
    let ffi = |e: heif_sys::HeifError| match e {
        heif_sys::HeifError::TooLarge => DecodeError::TooLarge(e.to_string()),
        heif_sys::HeifError::Cancelled => DecodeError::Cancelled,
        e => DecodeError::Malformed(e.to_string()),
    };
    let mut file = heif_sys::HeifFile::open(bytes, &limits).map_err(ffi)?;
    let info = file.item_info(base).map_err(ffi)?;
    let icc = file.item_icc(&info, 16 << 20);
    let (params, gain_id, tmap) = match source_kind {
        GainMapSource::Iso { tmap, gain } => {
            let data = file.item_data(tmap, MAX_METADATA).map_err(ffi)?;
            let iso =
                parse_iso(&data).ok_or_else(|| DecodeError::Malformed("tmap metadata".into()))?;
            (Params::Iso(iso), gain, Some(tmap))
        }
        GainMapSource::Apple { gain, xmp } => {
            let from_xmp = xmp
                .and_then(|x| file.item_data(x, MAX_METADATA).ok())
                .and_then(|x| xmp_headroom(&x));
            let headroom = from_xmp
                .or_else(|| {
                    let exif = meta.exif_of(base)?;
                    makernote_headroom(&file.item_data(exif, MAX_METADATA).ok()?)
                })
                .ok_or_else(|| DecodeError::Malformed("no Apple gain-map headroom".into()))?;
            (Params::Apple(headroom), gain, None)
        }
    };
    let base_img = file.decode_item(base, &ctl).map_err(ffi)?;
    cx.check_cancel()?;
    let gain_img = file.decode_item(gain_id, &ctl).map_err(ffi)?;
    drop(lease);
    cx.check_dims(base_img.width as usize, base_img.height as usize, 8, "HEIF")?;

    // The colour spaces, as RGB -> XYZ (D65).
    let base_space = match info.nclx {
        Some(n)
            if source::from_nclx(n.transfer_characteristics, n.colour_primaries).1
                != Primaries::Other =>
        {
            hdr::rgb_to_xyz(source::from_nclx(n.transfer_characteristics, n.colour_primaries).1)
        }
        _ => icc
            .as_deref()
            .and_then(hdr::icc_rgb_to_xyz)
            .unwrap_or_else(|| hdr::rgb_to_xyz(Primaries::Bt709)),
    };
    let rec709 = hdr::rgb_to_xyz(Primaries::Bt709);
    let work_space: Mat = match (&params, tmap.and_then(|t| meta.nclx(t))) {
        (Params::Iso(m), Some((primaries, transfer))) if !m.use_base_colour_space => {
            match source::from_nclx(transfer, primaries).1 {
                Primaries::Other => base_space,
                p => hdr::rgb_to_xyz(p),
            }
        }
        _ => base_space,
    };
    let to_work = hdr::between(&base_space, &work_space);
    let to_709 = hdr::between(&work_space, &rec709);

    let mut pixels = composite(&base_img, &gain_img, &params, to_work, to_709);
    cx.progress(0.9);

    let mut img = DecodedImage {
        pixels: std::mem::take(&mut pixels),
        width: base_img.width,
        height: base_img.height,
        format: PixelFormat::Rgba16Float,
        source: SourceInfo::new(
            format,
            format.name(),
            if base_img.has_alpha { 4 } else { 3 },
            base_img.bit_depth,
        ),
        icc: None,
        alpha_opaque: false,
        downscaled_from: None,
        source_mips: None,
        layout: None,
        animation: None,
    };
    if let Some(n) = info.nclx {
        (img.source.transfer, img.source.primaries, _) =
            source::from_nclx(n.transfer_characteristics, n.colour_primaries);
    }
    img.source.hdr = HdrKind::GainMap;
    for t in meta
        .transforms(base)
        .into_iter()
        .chain(tmap.map_or_else(Vec::new, |t| meta.transforms(t)))
    {
        apply_transform(&mut img, t);
    }
    Ok(img)
}

/// The linear Rec.709 RGBA half-float buffer: the base through the sRGB curve, into the working
/// space, times its gain, into Rec.709.
fn composite(
    base: &heif_sys::Image,
    gain: &heif_sys::Image,
    params: &Params,
    to_work: [[f32; 3]; 3],
    to_709: [[f32; 3]; 3],
) -> Vec<u8> {
    let (bw, bh) = (base.width as usize, base.height as usize);
    let (gw, gh) = (gain.width as usize, gain.height as usize);
    let base_max = if base.is_16bit { 65535.0 } else { 255.0 };
    // The sRGB curve, per stored code.
    let decode_lut: Vec<f32> = (0..=base_max as u32)
        .map(|v| hdr::srgb_decode(v as f32 / base_max))
        .collect();
    let sample = |img: &heif_sys::Image, i: usize, c: usize| -> f32 {
        if img.is_16bit {
            u16::from_ne_bytes([img.pixels[(i * 4 + c) * 2], img.pixels[(i * 4 + c) * 2 + 1]])
                as f32
        } else {
            img.pixels[i * 4 + c] as f32
        }
    };
    let gain_max = if gain.is_16bit { 65535.0 } else { 255.0 };
    // Bilinear taps of the gain map for each base column and row: the map stretched over the
    // base's coded size, pixel centres aligned.
    let taps = |n: usize, m: usize| -> Vec<(usize, usize, f32)> {
        (0..n)
            .map(|x| {
                let u = ((x as f32 + 0.5) * m as f32 / n as f32 - 0.5).clamp(0.0, (m - 1) as f32);
                let x0 = u.floor() as usize;
                (x0, (x0 + 1).min(m - 1), u - x0 as f32)
            })
            .collect()
    };
    let (xs, ys) = (taps(bw, gw), taps(bh, gh));
    let weight = match params {
        Params::Iso(m) => {
            let target = m.base_headroom.max(m.alternate_headroom);
            if m.alternate_headroom == m.base_headroom {
                0.0
            } else {
                ((target - m.base_headroom) / (m.alternate_headroom - m.base_headroom))
                    .clamp(0.0, 1.0)
            }
        }
        Params::Apple(_) => 1.0,
    };
    let mat = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
    };

    let row_bytes = bw * 8;
    let mut out = vec![0u8; row_bytes * bh];
    budget::par_chunks_mut_heavy(&mut out, row_bytes, |offset, part| {
        let first = offset / row_bytes;
        for (r, row) in part.chunks_exact_mut(row_bytes).enumerate() {
            let y = first + r;
            let (y0, y1, fy) = ys[y];
            for (x, px) in row.as_chunks_mut::<8>().0.iter_mut().enumerate() {
                let i = y * bw + x;
                let (x0, x1, fx) = xs[x];
                let g: [f32; 3] = std::array::from_fn(|c| {
                    let a = sample(gain, y0 * gw + x0, c) * (1.0 - fx)
                        + sample(gain, y0 * gw + x1, c) * fx;
                    let b = sample(gain, y1 * gw + x0, c) * (1.0 - fx)
                        + sample(gain, y1 * gw + x1, c) * fx;
                    (a * (1.0 - fy) + b * fy) / gain_max
                });
                let lin: [f32; 3] =
                    std::array::from_fn(|c| decode_lut[sample(base, i, c) as usize]);
                let work = mat(&to_work, lin);
                let hdr = match params {
                    Params::Iso(m) => std::array::from_fn(|c| {
                        let gc = g[c].clamp(0.0, 1.0);
                        let gp = if m.gamma[c] == 1.0 {
                            gc
                        } else {
                            gc.powf((1.0 / m.gamma[c]) as f32)
                        };
                        let log2 = m.min[c] as f32 * (1.0 - gp) + m.max[c] as f32 * gp;
                        (work[c] + m.base_offset[c] as f32) * (log2 * weight as f32).exp2()
                            - m.alternate_offset[c] as f32
                    }),
                    Params::Apple(headroom) => {
                        let gl = hdr::srgb_decode(g[0].clamp(0.0, 1.0));
                        let k = 1.0 + (headroom - 1.0) * gl;
                        work.map(|v| v * k)
                    }
                };
                let rgb = mat(&to_709, hdr);
                let a = if base.has_alpha {
                    sample(base, i, 3) / base_max
                } else {
                    1.0
                };
                for (c, v) in rgb.into_iter().chain([a]).enumerate() {
                    px[c * 2..c * 2 + 2].copy_from_slice(&f16::from_f32(v).to_bits().to_ne_bytes());
                }
            }
        }
    });
    out
}

/// One `irot` / `imir` / `clap`, on any buffer. Rotations and mirrors are EXIF orientations.
fn apply_transform(img: &mut DecodedImage, t: Transform) {
    match t {
        // Counter-clockwise quarter turns: 90° is EXIF 8, 180° is 3, 270° is 6.
        Transform::Rotate(n) => crate::exif::apply(img, [1, 8, 3, 6][n as usize & 3]),
        // Axis 0 exchanges top and bottom (EXIF 4), 1 left and right (EXIF 2).
        Transform::Mirror(axis) => crate::exif::apply(img, if axis == 0 { 4 } else { 2 }),
        Transform::Crop {
            width,
            height,
            horiz_off,
            vert_off,
        } => {
            let f = |(n, d): (i64, i64)| if d == 0 { 0.0 } else { n as f64 / d as f64 };
            let (w, h) = (img.width as f64, img.height as f64);
            let (cw, ch) = (
                f(width).round().clamp(1.0, w),
                f(height).round().clamp(1.0, h),
            );
            // The aperture's centre is the image's centre plus the offset (libheif's reading).
            let left = (f(horiz_off) + (w - 1.0) / 2.0 - (cw - 1.0) / 2.0)
                .round()
                .clamp(0.0, w - cw);
            let top = (f(vert_off) + (h - 1.0) / 2.0 - (ch - 1.0) / 2.0)
                .round()
                .clamp(0.0, h - ch);
            let (left, top, cw, ch) = (left as usize, top as usize, cw as usize, ch as usize);
            let bpp = img.format.bytes_per_pixel();
            let row = img.width as usize * bpp;
            let mut out = Vec::with_capacity(cw * ch * bpp);
            for y in top..top + ch {
                out.extend_from_slice(
                    &img.pixels[y * row + left * bpp..y * row + (left + cw) * bpp],
                );
            }
            img.pixels = out;
            img.width = cw as u32;
            img.height = ch as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixtures_iso_metadata() {
        // gainmap_iso.heic's tmap: one channel, base colour space, headroom 0 -> 2 stops, gains
        // 0..2 stops, gamma 1, offsets 1e-4.
        let hex = "000000000040000000000000000100000002000000010000000000000001000000020000000100000001000000010000d1b7200000000000d1b720000000";
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let m = parse_iso(&bytes).unwrap();
        assert!(m.use_base_colour_space);
        assert_eq!((m.base_headroom, m.alternate_headroom), (0.0, 2.0));
        assert_eq!((m.min, m.max, m.gamma), ([0.0; 3], [2.0; 3], [1.0; 3]));
        assert!((m.base_offset[2] - 1e-4).abs() < 1e-6);
        // A version this doesn't read.
        let mut v1 = bytes.clone();
        v1[0] = 1;
        assert_eq!(parse_iso(&v1), None);
    }

    /// The base's Display P3 ICC profile (ImageIO's, 536 bytes) gives Display P3's matrix: the
    /// colorants, adapted from D50 back to D65, against the matrix built from P3's chromaticities.
    #[test]
    fn a_display_p3_profile_reads_as_display_p3() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/heif/gainmap_iso.heic"),
        )
        .unwrap();
        let meta = Meta::parse(crate::heif_meta::find_meta(&bytes).unwrap().0).unwrap();
        let icc = meta
            .properties(1)
            .find(|(k, b)| k == b"colr" && b.starts_with(b"prof"))
            .map(|(_, b)| &b[4..])
            .unwrap();
        let from_icc = hdr::icc_rgb_to_xyz(icc).unwrap();
        let p3 = hdr::rgb_to_xyz(Primaries::DisplayP3);
        for r in 0..3 {
            for c in 0..3 {
                assert!(
                    (from_icc[r][c] - p3[r][c]).abs() < 2e-3,
                    "{from_icc:?} vs {p3:?}"
                );
            }
        }
    }

    #[test]
    fn xmp_headroom_as_element_or_attribute() {
        assert_eq!(
            xmp_headroom(
                b"<HDRGainMap:HDRGainMapHeadroom>3.149829</HDRGainMap:HDRGainMapHeadroom>"
            ),
            Some(3.149829)
        );
        assert_eq!(
            xmp_headroom(b"HDRGainMap:HDRGainMapHeadroom=\"4\""),
            Some(4.0)
        );
        assert_eq!(xmp_headroom(b"nothing here"), None);
    }
}
