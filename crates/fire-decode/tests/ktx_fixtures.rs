//! KTX2 and KTX files from KTX-Software's own test resources (`fixtures/ktx/`, fetched by its
//! `fetch.sh`), through `decode`: what real encoders write, which `tests/ktx.rs`'s hand-built
//! files can't be — ETC1S under BasisLZ, UASTC under Zstandard, UASTC HDR, ETC2 and ASTC from
//! their encoders.
//!
//! The lossy encodings are held to the lossless image they were made from by mean absolute
//! error, per channel, over every texel; each bound sits a little above what this decoder
//! measured when it was set, so a regression shows and encoder noise doesn't. Run with
//! `--nocapture` to see the measurements.
#![cfg(feature = "ktx")]

use std::path::PathBuf;

use fire_decode::{DecodeOptions, DecodedImage, PixelFormat, SheetKind, decode};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ktx")
}

fn read(name: &str) -> DecodedImage {
    let bytes = std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    decode(&bytes, None, &DecodeOptions::viewer()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Mean absolute difference per 8-bit channel between two RGBA8 buffers, over the colour
/// channels only (`rgb_only`) or all four.
fn mae(a: &[u8], b: &[u8], rgb_only: bool) -> f64 {
    assert_eq!(a.len(), b.len());
    let lanes = if rgb_only { 3 } else { 4 };
    let (mut sum, mut n) = (0u64, 0u64);
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..lanes {
            sum += u64::from(pa[c].abs_diff(pb[c]));
            n += 1;
        }
    }
    sum as f64 / n as f64
}

/// `name` against `reference` (both decoded), level 0 and then every stored level they share,
/// each within `bound`.
fn close(name: &str, reference: &str, rgb_only: bool, bound: f64) {
    let (img, r) = (read(name), read(reference));
    assert_eq!((img.width, img.height), (r.width, r.height), "{name}: size");
    assert_eq!(img.format, PixelFormat::Rgba8Unorm, "{name}");
    let mut levels = vec![(&img.pixels, &r.pixels)];
    if let (Some(a), Some(b)) = (&img.source_mips, &r.source_mips) {
        levels.extend(a.iter().zip(b));
    }
    for (level, (a, b)) in levels.into_iter().enumerate() {
        let e = mae(a, b, rgb_only);
        println!("{name} vs {reference}, level {level}: {e:.3}");
        assert!(e <= bound, "{name} level {level}: {e:.3} > {bound}");
    }
}

/// Basis Universal, both codecs, and a Zstandard-supercompressed RGBA8, against the PNG they
/// were all encoded from.
#[test]
fn color_grid_in_every_supercompression_matches_its_png() {
    close("ktx2/color_grid_zstd_5.ktx2", "color_grid.png", true, 0.0);
    close(
        "ktx2/color_grid_uastc_zstd_5.ktx2",
        "color_grid.png",
        true,
        1.0,
    );
    close("ktx2/color_grid_blze.ktx2", "color_grid.png", true, 3.5);
    assert_eq!(read("ktx2/color_grid_blze.ktx2").source.name, "KTX2 ETC1S");
    assert_eq!(
        read("ktx2/color_grid_uastc_zstd_5.ktx2").source.name,
        "KTX2 UASTC"
    );
}

/// One 64x64 image with its full chain, four ways: the RGBA8 original and its ETC2, ASTC 4x4
/// and ETC1S encodings, level by level.
#[test]
fn a_mip_chain_matches_across_codecs() {
    let original = read("ktx2/r8g8b8a8_srgb_mip.ktx2");
    assert_eq!(original.source_mips.as_ref().map(Vec::len), Some(6));
    close(
        "ktx2/r8g8b8a8_srgb_mip_etc2.ktx2",
        "ktx2/r8g8b8a8_srgb_mip.ktx2",
        false,
        1.5,
    );
    close(
        "ktx2/r8g8b8a8_srgb_mip_astc.ktx2",
        "ktx2/r8g8b8a8_srgb_mip.ktx2",
        false,
        0.5,
    );
    close(
        "ktx2/r8g8b8a8_srgb_mip_blze.ktx2",
        "ktx2/r8g8b8a8_srgb_mip.ktx2",
        true,
        2.0,
    );
}

/// The same picture stored bottom-up (`ru`) and top-down (`rd`) comes back the same.
#[test]
fn both_orientations_come_back_upright() {
    let (up, down) = (
        read("ktx2/orient_up_metadata.ktx2"),
        read("ktx2/orient_down_metadata.ktx2"),
    );
    assert!(up.pixels == down.pixels, "orient_up and orient_down differ");
    // KTX says it as `S=r,T=u`; the RGB8 files hold the same picture as the RGBA8 KTX2 ones.
    for name in ["ktx/orient_up_metadata.ktx", "ktx/orient_down_metadata.ktx"] {
        assert_eq!(mae(&read(name).pixels, &up.pixels, true), 0.0, "{name}");
    }
}

// --- KTX (v1) ------------------------------------------------------------------------------

/// The same payloads in both containers decode to the same pixels: the arrays' blocks, and an
/// RGB8 chain (with KTX's row padding) against the RGBA8 one, level by level.
#[test]
fn ktx_and_ktx2_agree() {
    for name in [
        "bc3_unorm_array_7",
        "etc2_unorm_array_7",
        "astc_8x8_unorm_array_7",
    ] {
        let (v1, v2) = (
            read(&format!("ktx/{name}.ktx")),
            read(&format!("ktx2/{name}.ktx2")),
        );
        assert_eq!(v1.layout, v2.layout, "{name}");
        assert!(v1.pixels == v2.pixels, "{name}");
        assert_eq!(&v1.source.name[..4], "KTX ", "{name}");
    }
    close(
        "ktx/r8g8b8_srgb_mip.ktx",
        "ktx2/r8g8b8a8_srgb_mip.ktx2",
        true,
        0.0,
    );
    assert_eq!(
        read("ktx/r8g8b8_srgb_mip.ktx").source_mips.map(|m| m.len()),
        Some(6)
    );
}

/// A 270-texel RGB8 row is 810 bytes, padded to 812: read without the padding, every row would
/// start two bytes later than the last and the picture would shear. Each level has its own, and
/// the first mip has to be a halving of level 0, not of a sheared one.
#[test]
fn ktx_row_padding_is_skipped() {
    let img = read("ktx/not4_r8g8b8_srgb.ktx");
    assert_eq!((img.width, img.height), (270, 270));
    let mips = img.source_mips.as_ref().expect("a chain");
    assert_eq!(mips.len(), 8);
    let (w, h) = (135usize, 135usize);
    let at = |x: usize, y: usize, c: usize| u32::from(img.pixels[(y * 270 + x) * 4 + c]);
    let mut boxed = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let sum = at(2 * x, 2 * y, c)
                    + at(2 * x + 1, 2 * y, c)
                    + at(2 * x, 2 * y + 1, c)
                    + at(2 * x + 1, 2 * y + 1, c);
                boxed[(y * w + x) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    let e = mae(&mips[0], &boxed, true);
    println!("not4 mip 1 against a box of level 0: {e:.3}");
    assert!(e <= 2.0, "{e:.3}");
}

/// ETC1 and the three ETC2 formats, by their GL names; ETC1 and ETC2 RGB of the same picture are
/// alike, and the punch-through alpha punches through.
#[test]
fn ktx_etc_formats() {
    for (name, label, channels) in [
        ("ktx/etc1.ktx", "KTX ETC1", 3),
        ("ktx/etc2_rgb.ktx", "KTX ETC2 RGB", 3),
        ("ktx/etc2_rgba1.ktx", "KTX ETC2 RGBA1", 4),
        ("ktx/etc2_rgba8.ktx", "KTX ETC2 RGBA", 4),
    ] {
        let img = read(name);
        assert_eq!((img.source.name, img.source.channels), (label, channels));
    }
    let e = mae(
        &read("ktx/etc1.ktx").pixels,
        &read("ktx/etc2_rgb.ktx").pixels,
        true,
    );
    assert!(e <= 3.0, "{e:.3}");
    let rgba1 = read("ktx/etc2_rgba1.ktx");
    assert!(rgba1.pixels.as_chunks::<4>().0.iter().any(|p| p[3] == 0));
}

/// Khronos' EAC conformance images: one channel grey, two red-green, eleven bits reported, and
/// the signed ones re-centred to use the whole range.
#[test]
fn ktx_eac_conformance_images() {
    for (name, label, channels) in [
        ("ktx/conftestimage_R11_EAC.ktx", "KTX EAC R11", 1),
        ("ktx/conftestimage_SIGNED_R11_EAC.ktx", "KTX EAC R11", 1),
        ("ktx/conftestimage_RG11_EAC.ktx", "KTX EAC RG11", 3),
        ("ktx/conftestimage_SIGNED_RG11_EAC.ktx", "KTX EAC RG11", 3),
    ] {
        let img = read(name);
        assert_eq!(
            (img.source.name, img.source.channels, img.source.bit_depth),
            (label, channels, 11),
            "{name}"
        );
        let px = img.pixels.as_chunks::<4>().0;
        let (lo, hi) = px
            .iter()
            .fold((255, 0), |(lo, hi), p| (p[0].min(lo), p[0].max(hi)));
        assert_eq!((lo, hi), (0, 255), "{name}: the whole range");
        for p in px {
            if channels == 1 {
                assert!(p[1] == p[0] && p[2] == p[0], "{name}: grey");
            } else {
                assert_eq!(p[2], 0, "{name}: no blue");
            }
        }
    }
    let l8 = read("ktx/l8_unorm_metadata.ktx");
    assert_eq!((l8.source.name, l8.source.channels), ("KTX L8", 1));
}

/// A seven-layer array in three block formats: a 3x3 sheet each, its spare cells empty, and the
/// three alike.
#[test]
fn an_array_in_three_block_formats() {
    let bc3 = read("ktx2/bc3_unorm_array_7.ktx2");
    for (name, label) in [
        ("ktx2/bc3_unorm_array_7.ktx2", "KTX2 BC3"),
        ("ktx2/etc2_unorm_array_7.ktx2", "KTX2 ETC2 RGB"),
        ("ktx2/astc_8x8_unorm_array_7.ktx2", "KTX2 ASTC 8x8"),
    ] {
        let img = read(name);
        assert_eq!(img.source.name, label);
        let l = img.layout.expect("a sheet");
        assert_eq!(
            (l.cols, l.rows, l.frames, l.kind),
            (3, 3, 7, SheetKind::ArrayLayers)
        );
        assert_eq!((img.width, img.height), (768, 768));
        // The last cell of the sheet is no layer's.
        assert_eq!(&img.pixels[(767 * 768 + 767) * 4..], &[0, 0, 0, 0]);
    }
    for name in [
        "ktx2/etc2_unorm_array_7.ktx2",
        "ktx2/astc_8x8_unorm_array_7.ktx2",
    ] {
        let e = mae(&read(name).pixels, &bc3.pixels, true);
        println!("{name} vs BC3: {e:.3}");
        assert!(e <= 4.0, "{name}: {e:.3}");
    }
}

/// An array with its mips, and a volume of seven slices, which keeps none.
#[test]
fn an_array_keeps_its_chain_and_a_volume_keeps_none() {
    let arr = read("ktx2/r8g8b8a8_srgb_array_7_mip.ktx2");
    let l = arr.layout.unwrap();
    assert_eq!((l.frames, l.kind), (7, SheetKind::ArrayLayers));
    assert_eq!(arr.source_mips.as_ref().map(Vec::len), Some(4));
    let vol = read("ktx2/r8g8b8a8_srgb_3d_7.ktx2");
    let l = vol.layout.unwrap();
    assert_eq!((l.frames, l.kind), (7, SheetKind::VolumeSlices));
    assert!(vol.source_mips.is_none());
}

/// The premultiplied file, straightened, against its straight twin: alike wherever there is
/// enough alpha to have kept the colour. It is sRGB, and KTX-Software premultiplies in linear
/// light, as a GPU's sRGB sampler blends; divided out of the encoded values instead, this
/// measured 4.1 where it now measures 0.04.
#[test]
fn premultiplied_alpha_is_straightened() {
    let (p, s) = (
        read("ktx2/alpha_complex_premultiplied.ktx2"),
        read("ktx2/alpha_complex_straight.ktx2"),
    );
    let (mut sum, mut n) = (0u64, 0u64);
    for (a, b) in p
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(s.pixels.as_chunks::<4>().0)
    {
        if b[3] >= 64 {
            sum += (0..3).map(|c| u64::from(a[c].abs_diff(b[c]))).sum::<u64>();
            n += 3;
        }
    }
    let e = sum as f64 / n as f64;
    println!("premultiplied vs straight, alpha >= 64: {e:.3}");
    assert!(e <= 0.5, "{e:.3}");
}

/// UASTC HDR comes out half-float, with values past 1.0: it reaches the HDR path, not a clamp.
#[test]
fn uastc_hdr_is_half_float() {
    let img = read("ktx2/Desk_uastc_hdr4x4_zstd_15.ktx2");
    assert_eq!(img.source.name, "KTX2 UASTC HDR");
    assert_eq!(img.format, PixelFormat::Rgba16Float);
    assert_eq!((img.width, img.height), (644, 874));
    let halves: &[u16] = bytemuck::cast_slice(&img.pixels);
    // 0x3c00 is 1.0; a positive half above it is brighter than SDR white.
    let bright = halves
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[..3].iter().any(|&h| h < 0x7c00 && h > 0x3c00))
        .count();
    println!("Desk: {bright} texels above 1.0");
    assert!(bright > 1000, "{bright}");
}
