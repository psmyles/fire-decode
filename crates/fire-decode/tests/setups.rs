//! Step 3's API, end to end through `decode`: the named setups (FD5), output narrowing (FD4), the
//! area filter (FD6), frames and surfaces, per-setup limits, the EXIF switch, cancel and panics,
//! and what `source` says about the file.

use std::io::Cursor;

use fire_decode::{
    DecodeCtl, DecodeError, DecodeOptions, Filter, Fit, Format, Frames, HdrKind, Limits, Output,
    PixelFormat, Primaries, Sample, Surfaces, Transfer, decode, decode_with_progress, sniff,
};

fn encode(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).expect("encode");
    out.into_inner()
}

// --- the setups --------------------------------------------------------------------------------

#[test]
fn the_setups_are_fd5s_table() {
    let v = DecodeOptions::viewer();
    assert!(v.honor_exif && v.stored_mips);
    assert_eq!(
        (v.output, v.frames, v.surfaces),
        (Output::Native, Frames::All, Surfaces::Sheet)
    );
    assert_eq!(
        v.fit,
        Some(Fit {
            max: 16384,
            filter: Filter::Nearest
        })
    );
    assert_eq!(v.limits, Limits::VIEWER);

    let i = DecodeOptions::inspect();
    assert!(!i.honor_exif && !i.stored_mips && i.fit.is_none());
    assert_eq!(
        (i.output, i.frames, i.surfaces),
        (Output::Native, Frames::First, Surfaces::Sheet)
    );
    assert_eq!(i.limits, Limits::VIEWER);

    let t = DecodeOptions::texture();
    assert!(!t.honor_exif && !t.stored_mips && t.fit.is_none());
    assert_eq!(
        (t.output, t.frames, t.surfaces),
        (Output::Rgba8OrFloat, Frames::First, Surfaces::First)
    );
    assert_eq!(t.limits, Limits::TEXTURE);

    let b = DecodeOptions::thumbnail(256);
    assert!(!b.honor_exif && !b.stored_mips);
    assert_eq!(
        b.fit,
        Some(Fit {
            max: 256,
            filter: Filter::Area
        })
    );
    assert_eq!(
        (b.output, b.frames, b.surfaces),
        (Output::Rgba8, Frames::First, Surfaces::First)
    );
    assert_eq!(b.limits, Limits::TEXTURE);

    #[cfg(feature = "icc")]
    {
        assert!(v.honor_icc);
        assert!(!i.honor_icc && !t.honor_icc && !b.honor_icc);
    }
    assert_eq!(
        (Limits::VIEWER.max_dim, Limits::VIEWER.max_bytes),
        (131072, 4 << 30)
    );
    assert_eq!(
        (Limits::TEXTURE.max_dim, Limits::TEXTURE.max_bytes),
        (30000, 1 << 30)
    );
}

// --- output narrowing ---------------------------------------------------------------------------

fn png16(values: &[[u16; 4]]) -> Vec<u8> {
    let img = image::ImageBuffer::<image::Rgba<u16>, _>::from_fn(values.len() as u32, 1, |x, _| {
        image::Rgba(values[x as usize])
    });
    encode(
        image::DynamicImage::ImageRgba16(img),
        image::ImageFormat::Png,
    )
}

#[test]
fn sixteen_bit_narrows_to_eight_rounding_to_nearest() {
    let bytes = png16(&[[0, 128, 129, 65535], [32896, 32895, 257, 65280]]);
    let native = decode(&bytes, Some("png"), &DecodeOptions::inspect()).unwrap();
    assert_eq!(native.format, PixelFormat::Rgba16Unorm);
    for output in [Output::Rgba8, Output::Rgba8OrFloat] {
        let opts = DecodeOptions {
            output,
            ..DecodeOptions::inspect()
        };
        let img = decode(&bytes, Some("png"), &opts).unwrap();
        assert_eq!(img.format, PixelFormat::Rgba8Unorm);
        // round(v × 255 / 65535): 128 → 0.498 → 0, 129 → 0.502 → 1, 32896 → 128.004, 32895 → 128.
        assert_eq!(img.pixels, [0, 0, 1, 255, 128, 128, 1, 254]);
        assert_eq!(img.source.bit_depth, 16, "source keeps the file's depth");
    }
}

/// A 2x1 float RGBA OpenEXR.
fn exr_float(pixels: [(f32, f32, f32, f32); 2]) -> Vec<u8> {
    use exr::prelude::*;
    let img = Image::from_channels(
        (2, 1),
        SpecificChannels::rgba(|Vec2(x, _y): Vec2<usize>| pixels[x]),
    );
    let mut out = Cursor::new(Vec::new());
    img.write().to_buffered(&mut out).unwrap();
    out.into_inner()
}

#[test]
fn float_narrows_by_clamping_with_no_curve_and_rgba8_or_float_keeps_it() {
    let bytes = exr_float([(0.5, -1.0, 4.0, 1.0), (0.25, 1.0, 0.0, 0.5)]);
    let img = decode(&bytes, Some("exr"), &DecodeOptions::thumbnail(64)).unwrap();
    assert_eq!(img.format, PixelFormat::Rgba8Unorm);
    assert_eq!(img.pixels, [128, 0, 255, 255, 64, 255, 0, 128]);
    assert_eq!(img.source.sample, Sample::Float);
    assert_eq!(img.source.transfer, Transfer::Linear);

    let tex = decode(&bytes, Some("exr"), &DecodeOptions::texture()).unwrap();
    assert!(tex.format.is_hdr(), "texture() returns float as float");
}

// --- the area filter ----------------------------------------------------------------------------

#[test]
fn area_averages_stored_values() {
    // A 4x2 image whose 2x2 blocks average to known values: left (0+255+0+255)/4, right 100.
    let img = image::RgbImage::from_fn(4, 2, |x, y| {
        if x < 2 {
            image::Rgb([if (x + y) % 2 == 0 { 0 } else { 255 }, 10, 20])
        } else {
            image::Rgb([100, 100, 100])
        }
    });
    let bytes = encode(image::DynamicImage::ImageRgb8(img), image::ImageFormat::Png);
    let out = decode(&bytes, Some("png"), &DecodeOptions::thumbnail(2)).unwrap();
    assert_eq!((out.width, out.height), (2, 1));
    assert_eq!(out.downscaled_from, Some((4, 2)));
    // 127.5 rounds up; stored values, not linear light (which would give 188).
    assert_eq!(out.pixels, [128, 10, 20, 255, 100, 100, 100, 255]);
}

#[test]
fn area_weights_colour_by_alpha() {
    // Transparent red beside opaque blue: the average is blue at half alpha, with no red fringe.
    let img = image::RgbaImage::from_fn(2, 1, |x, _| {
        if x == 0 {
            image::Rgba([255, 0, 0, 0])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    });
    let bytes = encode(
        image::DynamicImage::ImageRgba8(img),
        image::ImageFormat::Png,
    );
    let out = decode(&bytes, Some("png"), &DecodeOptions::thumbnail(1)).unwrap();
    assert_eq!(out.pixels, [0, 0, 255, 128]);
}

#[test]
fn area_covers_fractional_spans() {
    // 3 → 2 columns: each output takes one and a half source pixels.
    let img = image::GrayImage::from_raw(3, 1, vec![0, 90, 180]).unwrap();
    let bytes = encode(
        image::DynamicImage::ImageLuma8(img),
        image::ImageFormat::Png,
    );
    let opts = DecodeOptions {
        fit: Some(Fit {
            max: 2,
            filter: Filter::Area,
        }),
        ..DecodeOptions::thumbnail(2)
    };
    let out = decode(&bytes, Some("png"), &opts).unwrap();
    // (0 + 0.5 × 90) / 1.5 = 30; (0.5 × 90 + 180) / 1.5 = 150.
    assert_eq!((out.width, out.height), (2, 1));
    assert_eq!(out.pixels[0], 30);
    assert_eq!(out.pixels[4], 150);
}

#[test]
fn a_large_area_reduction_matches_a_block_mean() {
    // 512x512 of a gradient to 64x64, every output a mean of exactly 8x8.
    let img = image::RgbaImage::from_fn(512, 512, |x, y| {
        image::Rgba([(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8, 255])
    });
    let bytes = encode(
        image::DynamicImage::ImageRgba8(img.clone()),
        image::ImageFormat::Png,
    );
    let out = decode(&bytes, Some("png"), &DecodeOptions::thumbnail(64)).unwrap();
    assert_eq!((out.width, out.height), (64, 64));
    for (oy, ox) in [(0, 0), (13, 47), (63, 63)] {
        for c in 0..3 {
            let mut sum = 0u32;
            for y in 0..8 {
                for x in 0..8 {
                    sum += img.get_pixel(ox * 8 + x, oy * 8 + y)[c] as u32;
                }
            }
            let want = (sum as f32 / 64.0).round() as u8;
            let got = out.pixels[((oy * 64 + ox) * 4) as usize + c];
            assert!(
                got.abs_diff(want) <= 1,
                "({ox},{oy}) channel {c}: {got} vs {want}"
            );
        }
    }
}

// --- frames ---------------------------------------------------------------------------------------

fn gif(w: u32, h: u32, frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
        for i in 0..frames {
            let buf = image::RgbaImage::from_pixel(w, h, image::Rgba([i as u8 * 60, 0, 0, 255]));
            enc.encode_frame(image::Frame::new(buf)).unwrap();
        }
    }
    out
}

#[test]
fn frames_first_takes_one_frame_and_all_takes_every_one() {
    let bytes = gif(4, 4, 3);
    let all = decode(&bytes, Some("gif"), &DecodeOptions::viewer()).unwrap();
    let anim = all.animation.expect("animated");
    assert_eq!(anim.frames.len(), 3);
    assert!(!anim.truncated);
    let first = decode(&bytes, Some("gif"), &DecodeOptions::inspect()).unwrap();
    assert!(first.animation.is_none());
    assert_eq!(first.pixels[0], 0, "frame 0");
}

#[test]
fn a_sequence_cut_short_by_the_cap_says_so() {
    // Three 8x8 frames are 768 bytes; a 600-byte budget keeps two.
    let opts = DecodeOptions {
        limits: Limits {
            max_dim: 64,
            max_bytes: 600,
        },
        ..DecodeOptions::viewer()
    };
    let img = decode(&gif(8, 8, 3), Some("gif"), &opts).unwrap();
    let anim = img.animation.expect("still animated");
    assert_eq!(anim.frames.len(), 2);
    assert!(anim.truncated);
}

// --- surfaces and stored mips ---------------------------------------------------------------------

fn cubemap(face: u32) -> Vec<u8> {
    use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: face,
        width: face,
        depth: None,
        format: DxgiFormat::R8G8B8A8_UNorm,
        mipmap_levels: Some(2),
        array_layers: Some(6),
        is_cubemap: true,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
        caps2: Default::default(),
    })
    .unwrap();
    // Each face's level 0 a solid colour of its index, so the first face is recognisable.
    let level0 = (face * face * 4) as usize;
    let layer = level0 + ((face / 2) * (face / 2) * 4) as usize;
    for (k, chunk) in dds.data.chunks_mut(layer).enumerate() {
        chunk[..level0].fill(10 + k as u8);
    }
    let mut out = Vec::new();
    dds.write(&mut out).unwrap();
    out
}

#[test]
fn surfaces_sheet_tiles_every_face_and_first_keeps_one() {
    let bytes = cubemap(4);
    let sheet = decode(&bytes, Some("dds"), &DecodeOptions::viewer()).unwrap();
    assert_eq!((sheet.width, sheet.height), (12, 8));
    assert!(sheet.layout.is_some());
    assert!(
        sheet.source_mips.is_some(),
        "the viewer keeps the file's mips"
    );

    let first = decode(&bytes, Some("dds"), &DecodeOptions::texture()).unwrap();
    assert_eq!((first.width, first.height), (4, 4));
    assert!(first.layout.is_none());
    assert!(first.source_mips.is_none(), "texture() ignores stored mips");
    assert!(
        first.pixels.chunks(4).all(|p| p[..3] == [10, 10, 10]),
        "face 0"
    );

    // fire-shellext's setup: the sheet, without the mips.
    let shell = DecodeOptions {
        frames: Frames::First,
        stored_mips: false,
        ..DecodeOptions::viewer()
    };
    let img = decode(&bytes, Some("dds"), &shell).unwrap();
    assert_eq!((img.width, img.height), (12, 8));
    assert!(img.source_mips.is_none());
}

// --- limits ---------------------------------------------------------------------------------------

#[test]
fn each_setup_has_its_own_limits() {
    let wide = encode(
        image::DynamicImage::ImageLuma8(image::GrayImage::new(30_001, 1)),
        image::ImageFormat::Png,
    );
    assert!(decode(&wide, Some("png"), &DecodeOptions::inspect()).is_ok());
    for opts in [DecodeOptions::texture(), DecodeOptions::thumbnail(256)] {
        assert!(matches!(
            decode(&wide, Some("png"), &opts),
            Err(DecodeError::TooLarge(_))
        ));
    }
}

// --- EXIF -------------------------------------------------------------------------------------------

/// A 16x8 JPEG whose EXIF says "rotate 90° clockwise".
fn rotated_jpeg() -> Vec<u8> {
    let jpeg = encode(
        image::DynamicImage::ImageRgb8(image::RgbImage::new(16, 8)),
        image::ImageFormat::Jpeg,
    );
    let mut app1 = b"Exif\0\0II\x2a\x00\x08\x00\x00\x00\x01\x00".to_vec();
    app1.extend_from_slice(&0x0112u16.to_le_bytes());
    app1.extend_from_slice(&3u16.to_le_bytes());
    app1.extend_from_slice(&1u32.to_le_bytes());
    app1.extend_from_slice(&6u32.to_le_bytes());
    app1.extend_from_slice(&0u32.to_le_bytes());
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&(app1.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[test]
fn exif_orientation_is_a_switch() {
    let bytes = rotated_jpeg();
    let upright = decode(&bytes, Some("jpg"), &DecodeOptions::viewer()).unwrap();
    assert_eq!((upright.width, upright.height), (8, 16));
    for opts in [
        DecodeOptions::inspect(),
        DecodeOptions::texture(),
        DecodeOptions::thumbnail(64),
    ] {
        let stored = decode(&bytes, Some("jpg"), &opts).unwrap();
        assert_eq!((stored.width, stored.height), (16, 8), "as stored");
    }
}

// --- cancel, progress, threads ----------------------------------------------------------------

#[test]
fn a_cancelled_decode_says_so() {
    let bytes = encode(
        image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8)),
        image::ImageFormat::Png,
    );
    let ctl = DecodeCtl {
        progress: &|_| {},
        cancel: &|| true,
    };
    assert!(matches!(
        decode_with_progress(&bytes, Some("png"), &DecodeOptions::viewer(), &ctl),
        Err(DecodeError::Cancelled)
    ));
}

#[test]
fn progress_only_rises() {
    // Radiance HDR reports as it reads.
    let hdr = encode(
        image::DynamicImage::ImageRgb32F(image::Rgb32FImage::new(64, 512)),
        image::ImageFormat::Hdr,
    );
    let seen = std::sync::Mutex::new(Vec::new());
    let ctl = DecodeCtl {
        progress: &|p| seen.lock().unwrap().push(p),
        cancel: &|| false,
    };
    decode_with_progress(&hdr, Some("hdr"), &DecodeOptions::viewer(), &ctl).unwrap();
    let seen = seen.into_inner().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    assert!(seen.iter().all(|p| (0.0..=1.0).contains(p)));
}

#[test]
fn max_threads_still_decodes_the_same() {
    let img = image::RgbaImage::from_fn(1024, 512, |x, y| {
        image::Rgba([x as u8, y as u8, (x ^ y) as u8, 255])
    });
    let bytes = encode(
        image::DynamicImage::ImageRgba8(img),
        image::ImageFormat::Png,
    );
    let all = decode(&bytes, Some("png"), &DecodeOptions::thumbnail(100)).unwrap();
    let one = DecodeOptions {
        max_threads: Some(1),
        ..DecodeOptions::thumbnail(100)
    };
    assert_eq!(
        decode(&bytes, Some("png"), &one).unwrap().pixels,
        all.pixels
    );
}

// --- formats, extensions and `source` --------------------------------------------------------------

#[test]
fn sniff_names_the_format_by_its_bytes() {
    let png = encode(
        image::DynamicImage::ImageRgb8(image::RgbImage::new(2, 2)),
        image::ImageFormat::Png,
    );
    assert_eq!(
        sniff(&png, Some("jpg")),
        Format::Png,
        "bytes beat the extension"
    );
    assert_eq!(sniff(b"8BPS\0\x01", None), Format::Psd);
    assert_eq!(sniff(b"DDS ", None), Format::Dds);
    assert_eq!(sniff(b"\0\0\0\x18ftypavif", None), Format::Avif);
    assert_eq!(sniff(b"\0\0\0\x18ftypheic", None), Format::Heic);
    assert_eq!(sniff(b"\0\0\0\x18ftypmif1", None), Format::Heif);
    assert_eq!(sniff(b"no magic here", Some("tga")), Format::Tga);
    assert_eq!(sniff(b"II\x2a\x00rest", Some("nef")), Format::CameraRaw);
    assert_eq!(sniff(b"no magic here", None), Format::Unknown);
}

#[test]
fn a_compiled_out_format_is_unsupported() {
    let psd = b"8BPS\0\x01\0\0\0\0\0\0\0\x03\0\0\0\x01\0\0\0\x01\0\x08\0\x03".to_vec();
    let r = decode(&psd, Some("psd"), &DecodeOptions::viewer());
    if cfg!(feature = "psd") {
        assert!(!matches!(r, Err(DecodeError::Unsupported(_))));
    } else {
        assert!(matches!(r, Err(DecodeError::Unsupported(Format::Psd))));
    }
    let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
    let r = decode(&heic, Some("heic"), &DecodeOptions::viewer());
    if !cfg!(feature = "heif") {
        assert!(matches!(r, Err(DecodeError::Unsupported(Format::Heic))));
    }
}

#[test]
fn source_describes_the_file_whatever_the_buffer() {
    let bytes = png16(&[[1, 2, 3, 4]]);
    let img = decode(&bytes, Some("png"), &DecodeOptions::thumbnail(8)).unwrap();
    assert_eq!(img.format, PixelFormat::Rgba8Unorm);
    let s = img.source;
    assert_eq!(
        (s.format, s.name, s.channels, s.bit_depth),
        (Format::Png, "PNG", 4, 16)
    );
    assert_eq!(
        (s.sample, s.transfer, s.primaries),
        (Sample::Unorm, Transfer::Srgb, Primaries::Bt709)
    );
    assert_eq!(s.hdr, HdrKind::None);
}

#[cfg(feature = "heif")]
mod heif {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/heif")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn nclx_fills_in_the_source() {
        let pq = decode(
            &fixture("pq_lossless.heic"),
            Some("heic"),
            &DecodeOptions::inspect(),
        )
        .unwrap();
        let s = pq.source;
        assert_eq!(
            (s.format, s.transfer, s.primaries, s.hdr),
            (Format::Heic, Transfer::Pq, Primaries::Bt2020, HdrKind::Pq)
        );
        assert_eq!(s.bit_depth, 10);
        let hlg = decode(
            &fixture("hlg_lossless.avif"),
            Some("avif"),
            &DecodeOptions::inspect(),
        )
        .unwrap();
        assert_eq!(
            (hlg.source.format, hlg.source.transfer, hlg.source.hdr),
            (Format::Avif, Transfer::Hlg, HdrKind::Hlg)
        );
        let p3 = decode(
            &fixture("p3_nclx.heic"),
            Some("heic"),
            &DecodeOptions::inspect(),
        )
        .unwrap();
        assert_eq!(
            (p3.source.transfer, p3.source.primaries),
            (Transfer::Srgb, Primaries::DisplayP3)
        );
    }

    /// FD5: Display P3 nclx with no profile is converted to sRGB by the viewer and left as stored
    /// by the other setups. The patch the fixture's generator computed: P3 (200, 120, 60) is sRGB
    /// (213, 115, 42).
    #[cfg(feature = "icc")]
    #[test]
    fn the_viewer_converts_p3_nclx_and_inspect_leaves_it() {
        let bytes = fixture("p3_nclx.heic");
        let at = |img: &fire_decode::DecodedImage| {
            let i = ((16 * img.width + 128 + 16) * 4) as usize;
            [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2]]
        };
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 4);
        let stored = decode(&bytes, Some("heic"), &DecodeOptions::inspect()).unwrap();
        assert!(near(at(&stored), [200, 120, 60]), "{:?}", at(&stored));
        let viewer = decode(&bytes, Some("heic"), &DecodeOptions::viewer()).unwrap();
        assert!(near(at(&viewer), [213, 115, 42]), "{:?}", at(&viewer));
        // Pure P3 green clips to sRGB green.
        let i = ((16 * viewer.width + 32 + 16) * 4) as usize;
        assert!(viewer.pixels[i] < 8 && viewer.pixels[i + 1] > 247);
    }

    #[test]
    fn heif_takes_the_setups_limits() {
        let opts = DecodeOptions {
            limits: Limits {
                max_dim: 100,
                max_bytes: 1 << 30,
            },
            ..DecodeOptions::inspect()
        };
        assert!(matches!(
            decode(&fixture("pq_lossless.heic"), Some("heic"), &opts),
            Err(DecodeError::TooLarge(_))
        ));
    }
}
