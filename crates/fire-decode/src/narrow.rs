//! Narrowing a decoded buffer to the [`Output`] a setup asks for (FD4). Runs after any fit, on the
//! canvas and every animation frame alike.

use crate::{DecodedImage, Output, PixelFormat};

/// Narrow `img` to `output`. `Native` and an already-8-bit buffer cost nothing.
pub(crate) fn apply(img: &mut DecodedImage, output: Output) {
    let narrow = match (output, img.format) {
        (Output::Native, _) | (_, PixelFormat::Rgba8Unorm) => false,
        (Output::Rgba8, _) => true,
        (Output::Rgba8OrFloat, f) => !f.is_hdr(),
    };
    if !narrow {
        return;
    }
    let from = img.format;
    let needed = (img.width as usize)
        .saturating_mul(img.height as usize)
        .saturating_mul(from.bytes_per_pixel());
    img.transform_buffers(needed, |pixels| *pixels = to_rgba8(pixels, from));
    img.format = PixelFormat::Rgba8Unorm;
}

/// One buffer in `from`'s layout, as RGBA8:
/// - **16-bit unorm rounds to nearest**, matching ImageMagick: `round(v × 255 / 65535)`;
/// - **float clamps to 0..1 and scales, with no transfer curve**: what `image`'s `to_rgba8()`
///   does with float, and bite's thumbnail path for EXR and Radiance HDR. NaN becomes 0.
fn to_rgba8(src: &[u8], from: PixelFormat) -> Vec<u8> {
    let in_bpp = from.bytes_per_pixel();
    let px = src.len() / in_bpp;
    let mut out = vec![0u8; px * 4];
    crate::par_chunks_mut(&mut out, 4, |offset, part| {
        let first = offset / 4;
        let src = &src[first * in_bpp..(first + part.len() / 4) * in_bpp];
        match from {
            PixelFormat::Rgba16Unorm => {
                for (o, s) in part.iter_mut().zip(src.as_chunks::<2>().0) {
                    *o = unorm16_to_8(u16::from_ne_bytes(*s));
                }
            }
            PixelFormat::Rgba16Float => {
                for (o, s) in part.iter_mut().zip(src.as_chunks::<2>().0) {
                    *o = float_to_8(exr::prelude::f16::from_bits(u16::from_ne_bytes(*s)).to_f32());
                }
            }
            PixelFormat::Rgba32Float => {
                for (o, s) in part.iter_mut().zip(src.as_chunks::<4>().0) {
                    *o = float_to_8(f32::from_ne_bytes(*s));
                }
            }
            PixelFormat::Rgba8Unorm => part.copy_from_slice(src),
        }
    });
    out
}

/// `round(v × 255 / 65535)`, exactly, in integers.
#[inline]
pub(crate) fn unorm16_to_8(v: u16) -> u8 {
    ((v as u32 * 255 + 32767) / 65535) as u8
}

/// Clamp to 0..1, scale and round. NaN is 0.
#[inline]
fn float_to_8(v: f32) -> u8 {
    // `as` saturates and sends NaN to 0, so only the scale and the rounding are explicit.
    (v * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_bit_rounds_to_nearest() {
        assert_eq!(unorm16_to_8(0), 0);
        assert_eq!(unorm16_to_8(65535), 255);
        // 128.5 / 255 of full scale is the boundary between 128 and 129.
        for v in 0..=65535u16 {
            let exact = v as f64 * 255.0 / 65535.0;
            assert_eq!(unorm16_to_8(v), exact.round() as u8, "{v}");
        }
    }

    #[test]
    fn float_clamps_with_no_curve() {
        assert_eq!(float_to_8(-1.0), 0);
        assert_eq!(float_to_8(0.5), 128);
        assert_eq!(float_to_8(1.0), 255);
        assert_eq!(float_to_8(40.0), 255);
        assert_eq!(float_to_8(f32::NAN), 0);
        assert_eq!(float_to_8(f32::INFINITY), 255);
    }
}
