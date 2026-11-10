//! Colour conversion to sRGB via lcms2 (C FFI), the `icc` feature. The backends extract the
//! embedded profile onto [`DecodedImage::icc`]; here the pixels are transformed into the sRGB
//! working space so a Display-P3 / Adobe-RGB / etc. image displays with correct colour on the
//! (sRGB) surface. A file with no profile but a wide-gamut nclx description (Display P3 or BT.2020
//! primaries, common in AVIF) is converted from those primaries (FD5). Best-effort: any failure
//! (bad profile, unsupported layout) leaves the pixels untouched, i.e. falls back to assuming the
//! data is already sRGB.

use crate::{DecodedImage, PixelFormat, Primaries, Transfer};

/// Transform `img`'s pixels into sRGB, in place, from its embedded ICC profile, or with none from
/// the wide-gamut primaries its nclx box declares.
///
/// No-op when there is neither, for HDR/float data (linear working space; our float backends
/// never carry an ICC), or for non-RGB profiles (our pixels are RGBA, so a CMYK/Gray profile
/// would be misapplied — we assume sRGB instead).
pub fn apply(img: &mut DecodedImage) {
    if img.format.is_hdr() {
        return;
    }
    // FFI safety boundary (§6/§15): a malformed profile must never unwind into and
    // crash the viewer process. catch_unwind + best-effort: on any failure we keep
    // the original pixels (sRGB assumption).
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ctx = lcms2::ThreadContext::new();
        let src = match img.icc.as_deref() {
            Some(icc) => lcms2::Profile::new_icc_context(&ctx, icc).ok(),
            None => nclx_profile(&ctx, img.source.primaries, img.source.transfer),
        };
        if let Some(src) = src {
            transform_to_srgb(img, &ctx, &src);
        }
    }));
}

/// An RGB profile for nclx-described colour with primaries other than sRGB's: those primaries
/// with D65 white, and sRGB's tone curve on both sides, so only the gamut moves. `None` for
/// BT.709 primaries (already sRGB's), primaries without a known table, and the HDR transfers,
/// whose conversion is step 6b's (FD17).
///
/// The SDR transfers other than sRGB's (BT.709's, BT.601's) are taken as sRGB's: they are shown
/// that way today, and converting them as well would change every such file's tones, not just
/// its colours.
fn nclx_profile(
    ctx: &lcms2::ThreadContext,
    primaries: Primaries,
    transfer: Transfer,
) -> Option<lcms2::Profile<lcms2::ThreadContext>> {
    use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ToneCurve};
    if !matches!(transfer, Transfer::Srgb | Transfer::Other) {
        return None;
    }
    let [r, g, b] = match primaries {
        Primaries::DisplayP3 => [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)],
        Primaries::Bt2020 => [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)],
        _ => return None,
    };
    let xy = |(x, y): (f64, f64)| CIExyY { x, y, Y: 1.0 };
    let triple = CIExyYTRIPLE {
        Red: xy(r),
        Green: xy(g),
        Blue: xy(b),
    };
    // The sRGB curve as an ICC parametric curve (type 4).
    let srgb =
        ToneCurve::new_parametric(4, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])
            .ok()?;
    Profile::new_rgb_context(ctx, &xy((0.3127, 0.3290)), &triple, &[&srgb, &srgb, &srgb]).ok()
}

fn transform_to_srgb(
    img: &mut DecodedImage,
    ctx: &lcms2::ThreadContext,
    src: &lcms2::Profile<lcms2::ThreadContext>,
) {
    use lcms2::{
        ColorSpaceSignature, DisallowCache, Flags, Intent, PixelFormat as Fmt, Profile,
        ThreadContext, Transform,
    };

    // The transform is shared by the threads `par_chunks_mut` runs it on, which lcms2 allows
    // only for a transform made in a `ThreadContext` with NO_CACHE (its `Sync` bound). The
    // one-pixel cache given up is worthless on photographic data anyway.
    // Only RGB(A) source profiles map cleanly onto our RGBA pixels.
    if src.color_space() != ColorSpaceSignature::RgbData {
        return;
    }
    let dst = Profile::new_srgb_context(ctx);
    // Perceptual: the usual choice for displaying photographic images. COPY_ALPHA so
    // the alpha channel passes through untouched (lcms only transforms color).
    let intent = Intent::Perceptual;
    let flags = Flags::COPY_ALPHA | Flags::NO_CACHE;

    if is_srgb_equivalent(ctx, src, &dst, intent) {
        return;
    }

    // The same conversion runs over the canvas and every animation frame
    // (`transform_buffers`): a converted canvas over unconverted frames would visibly
    // shift color the moment playback starts.
    let needed = (img.width as usize)
        .saturating_mul(img.height as usize)
        .saturating_mul(img.format.bytes_per_pixel());
    match img.format {
        PixelFormat::Rgba8Unorm => {
            let t: Transform<[u8; 4], [u8; 4], ThreadContext, DisallowCache> =
                match Transform::new_flags_context(
                    ctx,
                    src,
                    Fmt::RGBA_8,
                    &dst,
                    Fmt::RGBA_8,
                    intent,
                    flags,
                ) {
                    Ok(t) => t,
                    Err(_) => return,
                };
            img.transform_buffers(needed, |pixels| {
                crate::budget::par_chunks_mut_heavy(pixels, 4, |_, part| {
                    if let Ok(px) = bytemuck::try_cast_slice_mut::<u8, [u8; 4]>(part) {
                        t.transform_in_place(px);
                    }
                });
            });
        }
        PixelFormat::Rgba16Unorm => {
            let t: Transform<[u16; 4], [u16; 4], ThreadContext, DisallowCache> =
                match Transform::new_flags_context(
                    ctx,
                    src,
                    Fmt::RGBA_16,
                    &dst,
                    Fmt::RGBA_16,
                    intent,
                    flags,
                ) {
                    Ok(t) => t,
                    Err(_) => return,
                };
            // 16-bit pixels are native-endian u16 bytes; cast may fail on alignment,
            // in which case we skip (sRGB assumption) rather than panic.
            img.transform_buffers(needed, |pixels| {
                crate::budget::par_chunks_mut_heavy(pixels, 8, |_, part| {
                    if let Ok(px) = bytemuck::try_cast_slice_mut::<u8, [u16; 4]>(part) {
                        t.transform_in_place(px);
                    }
                });
            });
        }
        // Float is handled by the is_hdr() early-out in apply().
        _ => {}
    }
}

/// Whether converting from `src` to sRGB would leave every colour within one 8-bit code of
/// where it started — i.e. the embedded profile *is* sRGB, as it is in every Photoshop export
/// and most camera JPEGs. Running lcms over such an image costs ~9 ns a pixel (over half a
/// second at 8192²) to move a fraction of a percent of colours by a single code: that is the
/// quantization of the profile's sampled tone curve against lcms's parametric one, not a
/// colour change, so the transform is skipped.
///
/// Probed rather than recognized by name or hash, because sRGB ships under dozens of
/// descriptions and byte layouts. The probe is a 17³ lattice (every 16th code, plus 255) for
/// the matrix and full 256-step ramps — grey and each primary alone — for the tone curves,
/// ~6k colours for ~0.1 ms. A matrix-shaper profile that differs from sRGB by more than one
/// code anywhere differs on that probe, because both of its parts are smooth.
fn is_srgb_equivalent(
    ctx: &lcms2::ThreadContext,
    src: &lcms2::Profile<lcms2::ThreadContext>,
    dst: &lcms2::Profile<lcms2::ThreadContext>,
    intent: lcms2::Intent,
) -> bool {
    use lcms2::{PixelFormat as Fmt, Transform};

    let Ok(t) = Transform::<[u8; 3], [u8; 3], _>::new_context(
        ctx,
        src,
        Fmt::RGB_8,
        dst,
        Fmt::RGB_8,
        intent,
    ) else {
        return false;
    };
    let lattice = (0..=16u32).map(|i| (i * 16).min(255) as u8);
    let mut probe: Vec<[u8; 3]> = Vec::with_capacity(17 * 17 * 17 + 4 * 256);
    for r in lattice.clone() {
        for g in lattice.clone() {
            probe.extend(lattice.clone().map(|b| [r, g, b]));
        }
    }
    for v in 0..=255u8 {
        probe.extend([[v, v, v], [v, 0, 0], [0, v, 0], [0, 0, v]]);
    }
    let mut out = probe.clone();
    t.transform_in_place(&mut out);
    probe
        .iter()
        .zip(&out)
        .all(|(a, b)| (0..3).all(|c| a[c].abs_diff(b[c]) <= 1))
}

/// [`crate::icc_is_srgb`]. Inside `catch_unwind` for the same reason [`apply`] is: the profile
/// comes from the file.
pub fn is_srgb(profile: &[u8]) -> bool {
    use lcms2::{ColorSpaceSignature, Intent, Profile, ThreadContext};

    std::panic::catch_unwind(|| {
        let ctx = ThreadContext::new();
        let Ok(src) = Profile::new_icc_context(&ctx, profile) else {
            return false;
        };
        let dst = Profile::new_srgb_context(&ctx);
        let intent = Intent::RelativeColorimetric;
        match src.color_space() {
            ColorSpaceSignature::RgbData => is_srgb_equivalent(&ctx, &src, &dst, intent),
            ColorSpaceSignature::GrayData => grey_is_srgb(&ctx, &src, &dst, intent),
            _ => false,
        }
    })
    .unwrap_or(false)
}

/// Whether every 8-bit grey level of a greyscale profile converts to the same sRGB grey, within
/// one code.
fn grey_is_srgb(
    ctx: &lcms2::ThreadContext,
    src: &lcms2::Profile<lcms2::ThreadContext>,
    dst: &lcms2::Profile<lcms2::ThreadContext>,
    intent: lcms2::Intent,
) -> bool {
    use lcms2::{PixelFormat as Fmt, Transform};

    let Ok(t) =
        Transform::<u8, [u8; 3], _>::new_context(ctx, src, Fmt::GRAY_8, dst, Fmt::RGB_8, intent)
    else {
        return false;
    };
    let ramp: Vec<u8> = (0..=255).collect();
    let mut out = vec![[0u8; 3]; ramp.len()];
    t.transform_pixels(&ramp, &mut out);
    ramp.iter()
        .zip(&out)
        .all(|(&v, rgb)| rgb.iter().all(|&c| c.abs_diff(v) <= 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img8(pixels: Vec<u8>) -> DecodedImage {
        let n = (pixels.len() / 4) as u32;
        DecodedImage {
            pixels,
            width: n,
            height: 1,
            format: PixelFormat::Rgba8Unorm,
            source: crate::SourceInfo::new(crate::Format::Unknown, "test", 4, 8),
            icc: None,
            alpha_opaque: false,
            downscaled_from: None,
            source_mips: None,
            layout: None,
            animation: None,
        }
    }

    #[test]
    fn no_icc_is_noop() {
        let mut img = img8(vec![10, 20, 30, 40, 200, 150, 100, 255]);
        let before = img.pixels.clone();
        apply(&mut img);
        assert_eq!(img.pixels, before);
    }

    #[test]
    fn srgb_profile_near_identity_and_preserves_alpha() {
        let icc = lcms2::Profile::new_srgb().icc().unwrap();
        let mut img = img8(vec![10, 20, 30, 40, 200, 150, 100, 255, 0, 128, 255, 77]);
        img.icc = Some(icc);
        let before = img.pixels.clone();
        apply(&mut img);
        // sRGB -> sRGB is (near) identity; allow tiny rounding on the color channels.
        for (i, (a, b)) in img.pixels.iter().zip(&before).enumerate() {
            assert!(
                (*a as i32 - *b as i32).abs() <= 2,
                "channel {i} drifted: {a} vs {b}"
            );
        }
        // COPY_ALPHA: alpha bytes (every 4th) preserved exactly.
        assert_eq!(
            [img.pixels[3], img.pixels[7], img.pixels[11]],
            [40, 255, 77]
        );
    }

    #[test]
    fn malformed_profile_is_safe_noop() {
        let mut img = img8(vec![10, 20, 30, 40]);
        img.icc = Some(vec![0, 1, 2, 3, 4, 5]); // not a valid ICC profile
        let before = img.pixels.clone();
        apply(&mut img); // must not panic
        assert_eq!(img.pixels, before);
    }

    /// sRGB primaries with `trc` on all three channels — the shape of every matrix-shaper
    /// profile these tests need.
    fn srgb_primaries_with(trc: &lcms2::ToneCurve) -> Vec<u8> {
        use lcms2::{CIExyY, CIExyYTRIPLE, Profile};
        let xy = |x, y| CIExyY { x, y, Y: 1.0 };
        let primaries = CIExyYTRIPLE {
            Red: xy(0.640, 0.330),
            Green: xy(0.300, 0.600),
            Blue: xy(0.150, 0.060),
        };
        Profile::new_rgb(&xy(0.3127, 0.3290), &primaries, &[trc, trc, trc])
            .unwrap()
            .icc()
            .unwrap()
    }

    /// A profile that *is* sRGB but samples its tone curve into a table (as the sRGB profile
    /// Windows ships does) lands within one code of identity, so it is not run at all: the
    /// pixels come back exactly as decoded.
    #[test]
    fn tabulated_srgb_profile_is_skipped() {
        let srgb = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let table: Vec<u16> = (0..1024)
            .map(|i| (srgb(i as f32 / 1023.0) * 65535.0).round() as u16)
            .collect();
        let icc = srgb_primaries_with(&lcms2::ToneCurve::new_tabulated(&table));
        let pixels: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 2, 200]).collect();
        let mut img = img8(pixels.clone());
        img.icc = Some(icc);
        apply(&mut img);
        assert_eq!(img.pixels, pixels);
    }

    /// The transform runs on several threads for a large image; the result must be exactly
    /// what one lcms call over the whole buffer produces.
    #[test]
    fn threaded_transform_matches_a_single_pass() {
        use lcms2::{Flags, Intent, PixelFormat as Fmt, Profile, Transform};

        let icc = srgb_primaries_with(&lcms2::ToneCurve::new(1.0)); // linear: far from sRGB
        // 2 MiB: comfortably past the threshold where the work is split.
        let pixels: Vec<u8> = (0..512 * 1024u32)
            .flat_map(|i| {
                [
                    i as u8,
                    (i >> 8) as u8,
                    (i >> 16) as u8 ^ 0x5a,
                    (i >> 3) as u8,
                ]
            })
            .collect();
        let mut expected: Vec<[u8; 4]> = bytemuck::cast_slice(&pixels).to_vec();
        let t: Transform<[u8; 4], [u8; 4]> = Transform::new_flags(
            &Profile::new_icc(&icc).unwrap(),
            Fmt::RGBA_8,
            &Profile::new_srgb(),
            Fmt::RGBA_8,
            Intent::Perceptual,
            Flags::COPY_ALPHA,
        )
        .unwrap();
        t.transform_in_place(&mut expected);

        let mut img = img8(pixels.clone());
        img.icc = Some(icc);
        apply(&mut img);
        assert_ne!(
            img.pixels, pixels,
            "a linear profile must actually be applied"
        );
        assert_eq!(img.pixels, bytemuck::cast_slice::<[u8; 4], u8>(&expected));
    }

    /// Proves the embedded transfer curve is actually applied (not just channel
    /// shuffling): a profile identical to sRGB except with a *linear* TRC means a
    /// mid-gray 128 is linear-light 0.5, which sRGB-encodes to ~188. So the gray must
    /// move substantially upward after the transform.
    #[test]
    fn linear_rgb_profile_applies_tone_curve() {
        use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ToneCurve};

        let d65 = CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.640,
                y: 0.330,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.300,
                y: 0.600,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.150,
                y: 0.060,
                Y: 1.0,
            },
        };
        let linear = ToneCurve::new(1.0);
        let profile = Profile::new_rgb(&d65, &primaries, &[&linear, &linear, &linear]).unwrap();
        let icc = profile.icc().unwrap();

        let mut img = img8(vec![128, 128, 128, 200]);
        img.icc = Some(icc);
        apply(&mut img);

        // Linear 0.5 -> sRGB ~= 188. Allow a generous window; the point is "much higher".
        assert!(
            img.pixels[0] >= 175 && img.pixels[0] <= 200,
            "linear 128 should sRGB-encode to ~188, got {}",
            img.pixels[0]
        );
        // Stays neutral gray and alpha is untouched.
        assert_eq!(img.pixels[0], img.pixels[1]);
        assert_eq!(img.pixels[1], img.pixels[2]);
        assert_eq!(img.pixels[3], 200);
    }

    /// The sRGB transfer curve as an ICC parametric curve (type 4).
    fn srgb_trc() -> lcms2::ToneCurve {
        lcms2::ToneCurve::new_parametric(
            4,
            &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045],
        )
        .unwrap()
    }

    /// An RGB profile with D65 white and the given primaries and tone curve.
    fn rgb_profile([r, g, b]: [(f64, f64); 3], trc: &lcms2::ToneCurve) -> Vec<u8> {
        use lcms2::{CIExyY, CIExyYTRIPLE, Profile};
        let xy = |(x, y): (f64, f64)| CIExyY { x, y, Y: 1.0 };
        let primaries = CIExyYTRIPLE {
            Red: xy(r),
            Green: xy(g),
            Blue: xy(b),
        };
        Profile::new_rgb(&xy((0.3127, 0.3290)), &primaries, &[trc, trc, trc])
            .unwrap()
            .icc()
            .unwrap()
    }

    #[test]
    fn srgb_profiles_are_srgb() {
        assert!(crate::icc_is_srgb(
            &lcms2::Profile::new_srgb().icc().unwrap()
        ));
        let srgb = [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)];
        assert!(crate::icc_is_srgb(&rgb_profile(srgb, &srgb_trc())));
    }

    /// Display P3 shares sRGB's tone curve and differs only in its primaries, so this is the
    /// case the colour probe exists for. Adobe RGB differs in both.
    #[test]
    fn wide_gamut_profiles_are_not_srgb() {
        let p3 = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
        assert!(!crate::icc_is_srgb(&rgb_profile(p3, &srgb_trc())));
        let adobe = [(0.640, 0.330), (0.210, 0.710), (0.150, 0.060)];
        let gamma = lcms2::ToneCurve::new(563.0 / 256.0);
        assert!(!crate::icc_is_srgb(&rgb_profile(adobe, &gamma)));
    }

    /// A greyscale profile is sRGB when its curve is sRGB's; a plain gamma 2.2 is not, and
    /// neither is a colour space we have no probe for.
    #[test]
    fn grey_and_other_profiles() {
        use lcms2::{CIExyY, Profile, ToneCurve};
        let d65 = CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let grey = |trc: &ToneCurve| Profile::new_gray(&d65, trc).unwrap().icc().unwrap();
        assert!(crate::icc_is_srgb(&grey(&srgb_trc())));
        assert!(!crate::icc_is_srgb(&grey(&ToneCurve::new(2.2))));
        assert!(!crate::icc_is_srgb(&Profile::new_xyz().icc().unwrap()));
        assert!(!crate::icc_is_srgb(&[0, 1, 2, 3, 4, 5]));
        assert!(!crate::icc_is_srgb(&[]));
    }
}
