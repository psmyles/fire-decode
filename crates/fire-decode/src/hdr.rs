//! PQ and HLG (fire-decode plan, FD17; step 6b): the HDR transfers decoded to light.
//!
//! libheif hands a PQ or HLG image back as its encoded signal: the YCbCr matrix applied, but
//! neither the transfer nor the BT.2020 primaries (measured in step 2b). Shown as sRGB that looks
//! flat and dark. So the crate decodes it to light itself, the same way for both [`Hdr`] modes:
//!
//! - **PQ:** the SMPTE ST 2084 EOTF gives absolute cd/m².
//! - **HLG:** BT.2100 display light: the inverse OETF to scene light, then the OOTF of a
//!   1000 cd/m² display (system gamma 1.2) on the luminance of the source's primaries.
//! - **Scale:** divided by 203 cd/m², BT.2408's reference white, so SDR white is 1.0.
//! - **Primaries:** converted to Rec.709 by matrix, in linear light. A colour outside Rec.709
//!   keeps its out-of-range values, negatives included (fire's tone curves clamp them, step 6a).
//!
//! Then [`Hdr::Linear`] keeps that as `Rgba16Float`, and [`Hdr::Sdr`] clips each channel to
//! 0..1 (203 cd/m² is white, anything brighter is lost) and sRGB-encodes it into
//! `Rgba16Unorm`, for the output setting to narrow as it narrows any 16-bit image. There is no
//! tone curve in `Sdr`: the highlights above reference white are what an SDR rendering gives up.
//!
//! Either way the image is Rec.709 afterwards, so any ICC profile the file carried no longer
//! describes it and is dropped. The nclx description stays on [`SourceInfo`](crate::SourceInfo),
//! which describes the file.

use exr::prelude::f16;

use crate::{Cx, DecodeError, DecodedImage, Hdr, HdrKind, PixelFormat, Primaries, budget};

/// BT.2408's reference white, in cd/m²: 1.0 in a `Linear` buffer, white in an `Sdr` one.
const REFERENCE_WHITE: f64 = 203.0;
/// HLG's nominal peak (BT.2100's reference display) and its system gamma at that peak.
const HLG_PEAK: f64 = 1000.0;
const HLG_GAMMA: f64 = 1.2;

/// Decodes `img`'s PQ or HLG signal to light, as the module describes. A no-op for any other
/// source. `img` is as libheif returned it: RGBA, 8 or 16 bits, the signal in the source's
/// primaries.
pub(crate) fn convert(img: &mut DecodedImage, hdr: Hdr, cx: &Cx) -> Result<(), DecodeError> {
    let kind = img.source.hdr;
    if !matches!(kind, HdrKind::Pq | HdrKind::Hlg) {
        return Ok(());
    }
    let to_709 = to_rec709(img.source.primaries);
    let luma = luminance(img.source.primaries);
    // Signal -> light for every 16-bit code: absolute light / 203 for PQ, scene light (0..1) for
    // HLG, whose display light also needs the pixel's other channels.
    let lut: Vec<f32> = (0..=u16::MAX)
        .map(|v| {
            let e = v as f64 / 65535.0;
            match kind {
                HdrKind::Pq => (pq_eotf(e) / REFERENCE_WHITE) as f32,
                _ => hlg_inverse_oetf(e) as f32,
            }
        })
        .collect();
    let hlg_scale = (HLG_PEAK / REFERENCE_WHITE) as f32;
    let hlg = kind == HdrKind::Hlg;

    // One pixel: the source's RGBA as 16-bit codes in, the output's eight bytes out.
    let pixel = |rgba: [u16; 4], out: &mut [u8]| {
        let mut e = [0.0f32; 3];
        for c in 0..3 {
            e[c] = lut[rgba[c] as usize];
        }
        if hlg {
            // The OOTF: Fd = peak * Ys^(gamma - 1) * E, Ys the scene luminance.
            let ys = luma[0] * e[0] + luma[1] * e[1] + luma[2] * e[2];
            let gain = if ys > 0.0 {
                hlg_scale * ys.powf((HLG_GAMMA - 1.0) as f32)
            } else {
                0.0
            };
            e = e.map(|v| v * gain);
        }
        let rgb: [f32; 3] = std::array::from_fn(|r| {
            to_709[r][0] * e[0] + to_709[r][1] * e[1] + to_709[r][2] * e[2]
        });
        match hdr {
            Hdr::Linear => {
                let a = rgba[3] as f32 / 65535.0;
                for (c, v) in rgb.into_iter().chain([a]).enumerate() {
                    out[c * 2..c * 2 + 2]
                        .copy_from_slice(&f16::from_f32(v).to_bits().to_ne_bytes());
                }
            }
            Hdr::Sdr => {
                for (c, v) in rgb.into_iter().enumerate() {
                    let s = srgb_encode(v.clamp(0.0, 1.0));
                    let code = (s * 65535.0 + 0.5) as u16;
                    out[c * 2..c * 2 + 2].copy_from_slice(&code.to_ne_bytes());
                }
                out[6..8].copy_from_slice(&rgba[3].to_ne_bytes());
            }
        }
    };

    match img.format {
        // Eight bytes in, eight out: in place.
        PixelFormat::Rgba16Unorm => {
            budget::par_chunks_mut_heavy(&mut img.pixels, 8, |_, part| {
                for px in part.as_chunks_mut::<8>().0 {
                    let rgba =
                        std::array::from_fn(|c| u16::from_ne_bytes([px[c * 2], px[c * 2 + 1]]));
                    pixel(rgba, px);
                }
            });
        }
        // An 8-bit PQ / HLG file is rare, but legal: widen into a new buffer.
        PixelFormat::Rgba8Unorm => {
            cx.check_dims(img.width as usize, img.height as usize, 8, "HEIF")?;
            let src = std::mem::take(&mut img.pixels);
            let mut out = vec![0u8; src.len() * 2];
            budget::par_chunks_mut_heavy(&mut out, 8, |offset, part| {
                let first = offset / 8;
                for (i, px) in part.as_chunks_mut::<8>().0.iter_mut().enumerate() {
                    let s = &src[(first + i) * 4..(first + i) * 4 + 4];
                    // 8 bits to 16: `v * 257`, so 255 is 65535.
                    pixel(std::array::from_fn(|c| s[c] as u16 * 257), px);
                }
            });
            img.pixels = out;
        }
        _ => return Ok(()),
    }
    img.format = match hdr {
        Hdr::Linear => PixelFormat::Rgba16Float,
        Hdr::Sdr => PixelFormat::Rgba16Unorm,
    };
    img.icc = None;
    Ok(())
}

/// RGB -> XYZ (D65) for an ICC matrix-shaper profile, from its `rXYZ` / `gXYZ` / `bXYZ`
/// colorants: the D50-adapted PCS values, adapted back to D65 with Bradford, as a colour-managed
/// viewer does. `None` for a profile that isn't RGB to XYZ with all three colorants. What an
/// iPhone's Display P3 base needs, which describes its colour by ICC, not nclx.
pub(crate) fn icc_rgb_to_xyz(icc: &[u8]) -> Option<Mat> {
    if icc.get(16..20)? != b"RGB " || icc.get(20..24)? != b"XYZ " {
        return None;
    }
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(icc.get(at..at + 4)?.try_into().ok()?))
    };
    let count = be32(128)? as usize;
    let colorant = |sig: &[u8; 4]| -> Option<[f64; 3]> {
        let entry = (0..count.min(1024))
            .map(|i| 132 + i * 12)
            .find(|&e| icc.get(e..e + 4) == Some(&sig[..]))?;
        let at = be32(entry + 4)? as usize;
        if icc.get(at..at + 4)? != b"XYZ " {
            return None;
        }
        let fixed = |o: usize| -> Option<f64> { Some(be32(at + 8 + o)? as i32 as f64 / 65536.0) };
        Some([fixed(0)?, fixed(4)?, fixed(8)?])
    };
    let [r, g, b] = [colorant(b"rXYZ")?, colorant(b"gXYZ")?, colorant(b"bXYZ")?];
    let d50: Mat = std::array::from_fn(|i| [r[i], g[i], b[i]]);
    // Bradford, D50 to D65.
    const BRADFORD: Mat = [
        [0.955_576_6, -0.023_039_3, 0.063_163_6],
        [-0.028_289_5, 1.009_941_6, 0.021_007_7],
        [0.012_298_2, -0.020_483, 1.329_909_8],
    ];
    Some(mul(&BRADFORD, &d50))
}

/// A linear RGB -> RGB matrix between two spaces given as RGB -> XYZ.
pub(crate) fn between(from: &Mat, to: &Mat) -> [[f32; 3]; 3] {
    mul(&invert(to), from).map(|row| row.map(|v| v as f32))
}

/// The sRGB transfer, decoded: stored 0..1 to linear light. SDR bases use it (Display P3's
/// curve is sRGB's).
pub(crate) fn srgb_decode(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// SMPTE ST 2084: signal (0..1) to absolute light, cd/m².
fn pq_eotf(e: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let p = e.clamp(0.0, 1.0).powf(1.0 / M2);
    10000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// BT.2100 HLG's inverse OETF: signal (0..1) to normalised scene light (0..1).
fn hlg_inverse_oetf(e: f64) -> f64 {
    const A: f64 = 0.178_832_77;
    const B: f64 = 1.0 - 4.0 * A;
    let c = 0.5 - A * (4.0 * A).ln();
    let e = e.clamp(0.0, 1.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - c) / A).exp() + B) / 12.0
    }
}

/// The sRGB encoding of linear light in 0..1.
fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// CIE xy of red, green and blue. Unknown primaries are taken as Rec.709's, as the rest of the
/// crate takes them.
fn chromaticities(p: Primaries) -> [(f64, f64); 3] {
    match p {
        Primaries::Bt2020 => [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)],
        Primaries::DisplayP3 => [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)],
        _ => [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)],
    }
}

pub(crate) type Mat = [[f64; 3]; 3];

/// RGB -> XYZ for the primaries, with D65 white (all three share it).
pub(crate) fn rgb_to_xyz(p: Primaries) -> Mat {
    let xyz = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let [r, g, b] = chromaticities(p).map(xyz);
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let white = xyz((0.3127, 0.3290));
    let s = mul_vec(&invert(&m), white);
    std::array::from_fn(|i| std::array::from_fn(|j| m[i][j] * s[j]))
}

/// Linear light in the source's primaries to linear Rec.709.
fn to_rec709(p: Primaries) -> [[f32; 3]; 3] {
    let m = mul(&invert(&rgb_to_xyz(Primaries::Bt709)), &rgb_to_xyz(p));
    m.map(|row| row.map(|v| v as f32))
}

/// The luminance (Y) weights of the primaries' red, green and blue: HLG's Ys.
fn luminance(p: Primaries) -> [f32; 3] {
    rgb_to_xyz(p)[1].map(|v| v as f32)
}

pub(crate) fn mul(a: &Mat, b: &Mat) -> Mat {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn mul_vec(a: &Mat, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}

pub(crate) fn invert(m: &Mat) -> Mat {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    [
        [
            (e * i - f * h) / det,
            (c * h - b * i) / det,
            (b * f - c * e) / det,
        ],
        [
            (f * g - d * i) / det,
            (a * i - c * g) / det,
            (c * d - a * f) / det,
        ],
        [
            (d * h - e * g) / det,
            (b * g - a * h) / det,
            (a * e - b * d) / det,
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_reference_points() {
        assert_eq!(pq_eotf(0.0), 0.0);
        assert!((pq_eotf(1.0) - 10000.0).abs() < 1e-6);
        // Code 594 of 1023 is the 203 cd/m² reference white, to the code's precision.
        assert!((pq_eotf(594.0 / 1023.0) - 202.9151).abs() < 1e-3);
    }

    #[test]
    fn hlg_reference_white_is_203_cd_m2() {
        // 75% signal on a 1000 cd/m² display: BT.2408's reference white.
        let e = hlg_inverse_oetf(0.75);
        let nits = HLG_PEAK * e.powf(HLG_GAMMA);
        assert!((nits - 203.0).abs() < 0.5, "{nits}");
        // The two halves of the curve meet at 0.5.
        assert!((hlg_inverse_oetf(0.5) - hlg_inverse_oetf(0.500_000_1)).abs() < 1e-6);
    }

    #[test]
    fn bt2020_to_rec709_matches_bt2087() {
        let m = to_rec709(Primaries::Bt2020);
        let want = [
            [1.660_491, -0.587_641, -0.072_85],
            [-0.124_55, 1.132_9, -0.008_349],
            [-0.018_151, -0.100_579, 1.118_73],
        ];
        for r in 0..3 {
            for c in 0..3 {
                assert!((m[r][c] - want[r][c]).abs() < 1e-4, "{m:?}");
            }
        }
        // White stays white, and Rec.709 converts to itself.
        let w: Vec<f32> = m.iter().map(|row| row.iter().sum()).collect();
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-5), "{w:?}");
        let id = to_rec709(Primaries::Bt709);
        assert!((id[0][0] - 1.0).abs() < 1e-6 && id[0][1].abs() < 1e-6);
    }

    #[test]
    fn bt2020_luminance_weights() {
        let y = luminance(Primaries::Bt2020);
        for (got, want) in y.iter().zip([0.2627, 0.6780, 0.0593]) {
            assert!((got - want).abs() < 1e-4, "{y:?}");
        }
    }
}
