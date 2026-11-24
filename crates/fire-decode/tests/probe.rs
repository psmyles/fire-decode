//! FD19 and step 3's gate: `probe()` reports the size and format `decode()` returns with
//! `DecodeOptions::inspect()`, for every format, without decoding.
//!
//! Files of every format are built here, so CI checks each; the HEIF fixtures are read from
//! `fixtures/heif/`. Real corpora are checked too when `FIRE_DECODE_CORPUS` names them
//! (directories, walked recursively, listed as `PATH` is: `:`-separated, `;` on Windows):
//!
//!   FIRE_DECODE_CORPUS=../bite/test_images:local/corpus \
//!     cargo test --release -p fire-decode --features psd,heif,icc,raw --test probe -- --nocapture

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use fire_decode::{DecodeError, DecodeOptions, Format, decode, probe};

/// Probe and decode `bytes`, and hold them to each other. `None` when it doesn't decode (and then
/// only the format is checked: a file that won't decode may still probe, e.g. a PSD with no
/// merged image).
fn check(name: &str, bytes: &[u8], ext: Option<&str>) -> Option<Format> {
    let probed = probe(Cursor::new(bytes), ext);
    match decode(bytes, ext, &DecodeOptions::inspect()) {
        Ok(img) => {
            let p = probed.unwrap_or_else(|e| panic!("{name}: decodes, but probe says {e}"));
            assert_eq!(
                (p.width, p.height),
                (img.width, img.height),
                "{name}: probe's size against decode's"
            );
            assert_eq!(p.format, img.source.format, "{name}: probe's format");
            Some(p.format)
        }
        Err(DecodeError::Unsupported(f)) => {
            assert!(
                matches!(probed, Err(DecodeError::Unsupported(g)) if g == f),
                "{name}: decode says {f:?} is compiled out, probe says {probed:?}"
            );
            None
        }
        Err(_) => None,
    }
}

fn encode(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).expect("encode");
    out.into_inner()
}

fn rgba(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x * 40) as u8, (y * 50) as u8, 90, 200 + (x % 2) as u8 * 55])
    }))
}

fn rgb(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x * 40) as u8, (y * 50) as u8, 90])
    }))
}

/// A JPEG whose EXIF says "rotate 90° clockwise" (orientation 6): `inspect()` leaves it as stored.
fn rotated_jpeg(w: u32, h: u32) -> Vec<u8> {
    let jpeg = encode(rgb(w, h), image::ImageFormat::Jpeg);
    // APP1: "Exif\0\0", then a little-endian TIFF header and an IFD with one Orientation entry.
    let mut tiff = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&6u32.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&(app1.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

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

fn exr(w: usize, h: usize) -> Vec<u8> {
    use exr::prelude::*;
    let img = Image::from_channels(
        (w, h),
        SpecificChannels::rgba(|Vec2(x, _y): Vec2<usize>| {
            (x as f32 / 8.0, 0.5f32, 0.25f32, 1.0f32)
        }),
    );
    let mut out = Cursor::new(Vec::new());
    img.write().to_buffered(&mut out).unwrap();
    out.into_inner()
}

fn dds(w: u32, h: u32, layers: Option<u32>, cube: bool) -> Vec<u8> {
    use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
    let dds = Dds::new_dxgi(NewDxgiParams {
        height: h,
        width: w,
        depth: None,
        format: DxgiFormat::R8G8B8A8_UNorm,
        mipmap_levels: Some(2),
        array_layers: layers,
        is_cubemap: cube,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
        caps2: Default::default(),
    })
    .unwrap();
    let mut out = Vec::new();
    dds.write(&mut out).unwrap();
    out
}

/// A KTX2 of `w`x`h` RGBA8 surfaces, `layers` deep (0 for none) with `faces` faces, two levels,
/// laid out by hand (the `ktx2` crate is only there with the `ktx` feature, and this file is
/// checked without it too). The data format descriptor is the basic block for
/// `R8G8B8A8_UNORM`: four 8-bit samples.
fn ktx2(w: u32, h: u32, layers: u32, faces: u32) -> Vec<u8> {
    let surfaces = layers.max(1) * faces;
    let level_bytes = |l: u32| ((w >> l).max(1) * (h >> l).max(1) * 4 * surfaces) as usize;
    let mut dfd = Vec::new();
    dfd.extend_from_slice(&92u32.to_le_bytes()); // dfdTotalSize
    dfd.extend_from_slice(&0u32.to_le_bytes()); // vendor 0, descriptor type 0: basic
    dfd.extend_from_slice(&2u16.to_le_bytes()); // version
    dfd.extend_from_slice(&88u16.to_le_bytes()); // block size: 24 + 4 samples
    dfd.extend_from_slice(&[1, 1, 2, 0]); // RGBSDA, BT.709, sRGB, straight alpha
    dfd.extend_from_slice(&[0; 4]); // a 1x1x1x1 texel block
    dfd.extend_from_slice(&[4, 0, 0, 0, 0, 0, 0, 0]); // 4 bytes in plane 0
    for (channel, offset) in [(0u8, 0u16), (1, 8), (2, 16), (15, 24)] {
        dfd.extend_from_slice(&offset.to_le_bytes());
        dfd.extend_from_slice(&[7, channel]); // 8 bits (stored less one), the channel
        dfd.extend_from_slice(&[0; 4]); // sample position
        dfd.extend_from_slice(&0u32.to_le_bytes());
        dfd.extend_from_slice(&255u32.to_le_bytes());
    }
    let dfd_off = 80 + 2 * 24;
    let mut out = vec![0u8; dfd_off];
    out.extend_from_slice(&dfd);
    let mut offsets = [0usize; 2];
    for l in [1, 0] {
        while out.len() % 16 != 0 {
            out.push(0);
        }
        offsets[l as usize] = out.len();
        out.extend((0..level_bytes(l)).map(|i| i as u8));
    }
    out.push(0);
    let mut h80 = vec![
        0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
    ];
    for v in [
        37,
        1,
        w,
        h,
        0,
        layers,
        faces,
        2,
        0,
        dfd_off as u32,
        92,
        0,
        0,
    ] {
        h80.extend_from_slice(&u32::to_le_bytes(v));
    }
    h80.extend_from_slice(&[0; 16]); // no supercompression global data
    out[..80].copy_from_slice(&h80);
    for (l, offset) in offsets.iter().enumerate() {
        let at = 80 + 24 * l;
        let len = level_bytes(l as u32) as u64;
        out[at..at + 8].copy_from_slice(&(*offset as u64).to_le_bytes());
        out[at + 8..at + 16].copy_from_slice(&len.to_le_bytes());
        out[at + 16..at + 24].copy_from_slice(&len.to_le_bytes());
    }
    out
}

/// A KTX (v1) of one `w`x`h` RGB8 level, `faces` faces (a lone cubemap when 6), rows padded to
/// 4 bytes as the format stores them.
fn ktx1(w: u32, h: u32, faces: u32) -> Vec<u8> {
    let row = (w * 3).next_multiple_of(4) as usize;
    let face = row * h as usize;
    let mut out = vec![
        0xAB, 0x4B, 0x54, 0x58, 0x20, 0x31, 0x31, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
    ];
    // Endianness, glType UNSIGNED_BYTE, glTypeSize, glFormat RGB, glInternalFormat RGB8,
    // glBaseInternalFormat, size, depth, array elements, faces, levels, key/value bytes.
    for v in [
        0x0403_0201,
        0x1401,
        1,
        0x1907,
        0x8051,
        0x1907,
        w,
        h,
        0,
        0,
        faces,
        1,
        0,
    ] {
        out.extend_from_slice(&u32::to_le_bytes(v));
    }
    out.extend_from_slice(&(face as u32).to_le_bytes());
    out.extend((0..face * faces as usize).map(|i| i as u8));
    out
}

/// A raw 8-bit RGB PSD with a merged image.
fn psd(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"8BPS".to_vec();
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&[0; 6]);
    b.extend_from_slice(&3u16.to_be_bytes()); // channels
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&8u16.to_be_bytes()); // depth
    b.extend_from_slice(&3u16.to_be_bytes()); // RGB
    b.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    b.extend_from_slice(&0u32.to_be_bytes()); // image resources
    b.extend_from_slice(&0u32.to_be_bytes()); // layer and mask info
    b.extend_from_slice(&0u16.to_be_bytes()); // raw
    b.extend((0..3 * w * h).map(|i| (i * 7) as u8));
    b
}

/// Every format, built in memory.
fn generated() -> Vec<(String, Vec<u8>, &'static str)> {
    use image::ImageFormat as I;
    let mut files: Vec<(String, Vec<u8>, &'static str)> = vec![
        ("png".into(), encode(rgba(13, 7), I::Png), "png"),
        ("jpg, EXIF-rotated".into(), rotated_jpeg(16, 8), "jpg"),
        ("gif, animated".into(), gif(5, 3, 3), "gif"),
        ("bmp 24-bit".into(), encode(rgb(7, 5), I::Bmp), "bmp"),
        ("bmp 32-bit".into(), encode(rgba(7, 5), I::Bmp), "bmp"),
        ("tga".into(), encode(rgb(9, 4), I::Tga), "tga"),
        ("tiff".into(), encode(rgb(6, 6), I::Tiff), "tif"),
        ("ico".into(), encode(rgba(16, 16), I::Ico), "ico"),
        (
            "hdr".into(),
            encode(
                image::DynamicImage::ImageRgb32F(image::Rgb32FImage::new(4, 3)),
                I::Hdr,
            ),
            "hdr",
        ),
        (
            "qoi".into(),
            qoi::encode_to_vec(vec![7u8; 5 * 5 * 4], 5, 5).unwrap(),
            "qoi",
        ),
        ("exr".into(), exr(10, 6), "exr"),
        ("dds".into(), dds(8, 4, None, false), "dds"),
        ("dds cubemap".into(), dds(4, 4, Some(6), true), "dds"),
        ("dds array".into(), dds(4, 4, Some(3), false), "dds"),
        ("ktx".into(), ktx1(5, 3, 1), "ktx"),
        ("ktx cubemap".into(), ktx1(4, 4, 6), "ktx"),
        ("ktx2".into(), ktx2(8, 4, 0, 1), "ktx2"),
        ("ktx2 cubemap".into(), ktx2(4, 4, 0, 6), "ktx2"),
        ("ktx2 array".into(), ktx2(4, 2, 3, 1), "ktx2"),
        // Its data runs well past the 4 KiB probe reads: the header alone has the size.
        (
            "ktx2 past the probe's head".into(),
            ktx2(64, 64, 0, 1),
            "ktx2",
        ),
        ("psd".into(), psd(5, 4), "psd"),
    ];
    // Samples that aren't whitespace: zune's PPM reader skips whitespace after the header.
    let mut ppm = b"P6\n3 2\n255\n".to_vec();
    ppm.extend([200u8; 18]);
    files.push(("ppm".into(), ppm, "ppm"));
    let mut pam =
        b"P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
    pam.extend([200u8; 8]);
    files.push(("pam".into(), pam, "pam"));
    let mut ff = b"farbfeld".to_vec();
    ff.extend_from_slice(&3u32.to_be_bytes());
    ff.extend_from_slice(&2u32.to_be_bytes());
    ff.extend([0u8; 3 * 2 * 8]);
    files.push(("farbfeld".into(), ff, "ff"));
    let mut webp = Vec::new();
    image_webp::WebPEncoder::new(&mut webp)
        .encode(&[1u8; 7 * 3 * 4], 7, 3, image_webp::ColorType::Rgba8)
        .unwrap();
    files.push(("webp lossless".into(), webp, "webp"));
    files
}

#[test]
fn every_generated_format_probes_as_it_decodes() {
    let mut seen = Vec::new();
    for (name, bytes, ext) in generated() {
        let format = check(&name, &bytes, Some(ext));
        // With no extension too, for every format with magic of its own.
        if ext != "tga" {
            check(&format!("{name}, no extension"), &bytes, None);
        }
        seen.push((name, format));
    }
    for (name, format) in &seen {
        let compiled_out = (name == "psd" && !cfg!(feature = "psd"))
            || (name.starts_with("ktx") && !cfg!(feature = "ktx"));
        assert!(
            format.is_some() || compiled_out,
            "{name} didn't decode, so probe wasn't checked"
        );
    }
}

#[test]
fn probe_leaves_exif_orientation_alone() {
    let p = probe(Cursor::new(rotated_jpeg(16, 8)), Some("jpg")).unwrap();
    assert_eq!((p.width, p.height, p.format), (16, 8, Format::Jpeg));
    // The viewer turns it upright; probe and `inspect()` report it as stored, as Unity imports it.
    let upright = decode(&rotated_jpeg(16, 8), Some("jpg"), &DecodeOptions::viewer()).unwrap();
    assert_eq!((upright.width, upright.height), (8, 16));
}

#[test]
fn a_dds_sheet_probes_at_its_sheet_size() {
    let p = probe(Cursor::new(dds(4, 4, Some(6), true)), None).unwrap();
    assert_eq!((p.width, p.height), (12, 8), "six 4x4 faces tile 3x2");
}

#[cfg(feature = "ktx")]
#[test]
fn a_ktx2_sheet_probes_at_its_sheet_size() {
    let p = probe(Cursor::new(ktx2(4, 4, 0, 6)), None).unwrap();
    assert_eq!((p.width, p.height, p.format), (12, 8, Format::Ktx2));
    let p = probe(Cursor::new(ktx2(4, 2, 3, 1)), None).unwrap();
    assert_eq!((p.width, p.height), (8, 4), "three 4x2 layers tile 2x2");
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    for bytes in [
        &b""[..],
        b"\x89PNG\r\n\x1a\n",
        b"\xFF\xD8\xFF\xE0",
        b"\0\0\0\x18ftypheic\0\0\0\0mif1heic",
        b"DDS ",
        b"8BPS",
        // KTX2's identifier with nothing after it.
        b"\xABKTX 20\xBB\r\n\x1A\n",
    ] {
        assert!(probe(Cursor::new(bytes), None).is_err(), "{bytes:?}");
    }
}

#[cfg(feature = "heif")]
#[test]
fn every_heif_fixture_probes_as_it_decodes() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/heif");
    let mut decoded = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
        if !matches!(ext.as_deref(), Some("heic" | "heif" | "avif")) {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if check(&name, &bytes, ext.as_deref()).is_some() {
            decoded += 1;
        }
    }
    assert!(decoded >= 25, "only {decoded} HEIF fixtures decoded");
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The corpora `FIRE_DECODE_CORPUS` names, if any: every file that decodes must probe the same.
#[test]
fn every_corpus_file_probes_as_it_decodes() {
    let Some(dirs) = std::env::var_os("FIRE_DECODE_CORPUS") else {
        eprintln!("FIRE_DECODE_CORPUS unset: no corpus checked");
        return;
    };
    let mut files = Vec::new();
    // The platform's list syntax, so a Windows drive letter's `:` doesn't split a path.
    for d in std::env::split_paths(&dirs).filter(|d| !d.as_os_str().is_empty()) {
        walk(&d, &mut files);
    }
    let mut by_format: BTreeMap<String, usize> = BTreeMap::new();
    let mut failed = 0;
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let ext = path.extension().and_then(|e| e.to_str());
        match check(&path.display().to_string(), &bytes, ext) {
            Some(f) => *by_format.entry(format!("{f:?}")).or_default() += 1,
            None => failed += 1,
        }
    }
    eprintln!(
        "corpus: {} files, {} not decoded; probed and matched: {by_format:?}",
        files.len(),
        failed
    );
}
