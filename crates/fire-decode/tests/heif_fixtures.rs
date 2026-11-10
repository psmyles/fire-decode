//! Every file in `fixtures/heif/` through `decode`, against what `expected.json` says it must
//! return (fire-decode plan, step 2b's gate): sizes and orientation, bit depths, grids, the first
//! frame of a sequence-only file, straightened alpha, and an error, never an abort, for every
//! malformed or oversized file.
#![cfg(feature = "heif")]

use std::path::PathBuf;

use fire_decode::{DecodeError, DecodeOptions, DecodedImage, decode};
use serde_json::Value;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/heif")
}

fn expected() -> serde_json::Map<String, Value> {
    let text = std::fs::read_to_string(fixtures().join("expected.json")).expect("expected.json");
    let v: Value = serde_json::from_str(&text).expect("expected.json parses");
    v["files"].as_object().expect("files").clone()
}

/// Full size, no ICC conversion: the pixels as libheif hands them over.
fn opts() -> DecodeOptions {
    DecodeOptions::inspect()
}

fn read(name: &str) -> Result<DecodedImage, DecodeError> {
    let bytes = std::fs::read(fixtures().join(name)).expect("read fixture");
    let ext = name.rsplit('.').next();
    decode(&bytes, ext, &opts())
}

/// Pixel `(x, y)` as RGBA in 0..1.
fn pixel(img: &DecodedImage, x: usize, y: usize) -> [f32; 4] {
    let i = (y * img.width as usize + x) * 4;
    match img.format {
        fire_decode::PixelFormat::Rgba16Unorm => {
            let s = |c: usize| {
                let b = &img.pixels[(i + c) * 2..(i + c) * 2 + 2];
                u16::from_ne_bytes([b[0], b[1]]) as f32 / 65535.0
            };
            [s(0), s(1), s(2), s(3)]
        }
        _ => {
            let s = |c: usize| img.pixels[i + c] as f32 / 255.0;
            [s(0), s(1), s(2), s(3)]
        }
    }
}

/// Each corner block holds its colour: the nearest of the four, within `tolerance`.
fn check_corners(name: &str, img: &DecodedImage, corners: &Value, tolerance: f32) {
    let b = corners["block"].as_u64().unwrap() as usize;
    let (w, h) = (img.width as usize, img.height as usize);
    let colours: Vec<(&str, [f32; 3])> = ["tl", "tr", "bl", "br"]
        .iter()
        .map(|k| {
            let c = &corners["colours"][*k];
            (*k, [0, 1, 2].map(|i| c[i].as_f64().unwrap() as f32))
        })
        .collect();
    let centres = [
        ("tl", b / 2, b / 2),
        ("tr", w - 1 - b / 2, b / 2),
        ("bl", b / 2, h - 1 - b / 2),
        ("br", w - 1 - b / 2, h - 1 - b / 2),
    ];
    for (corner, x, y) in centres {
        let p = pixel(img, x, y);
        let dist = |c: &[f32; 3]| (0..3).map(|i| (p[i] - c[i]).powi(2)).sum::<f32>();
        let nearest = colours
            .iter()
            .min_by(|a, b| dist(&a.1).total_cmp(&dist(&b.1)))
            .unwrap();
        let want = colours.iter().find(|c| c.0 == corner).unwrap();
        let off = (0..3).map(|i| (p[i] - want.1[i]).abs()).fold(0.0, f32::max);
        assert!(
            nearest.0 == corner && off < tolerance,
            "{name}: {corner} corner is {p:?}, expected {:?}",
            want.1
        );
    }
}

/// Fixtures whose expectation a later step meets, with the step. They are checked to still fail,
/// so the list can't go stale once the step lands.
const PENDING: &[(&str, &str)] = &[(
    "gainmap_iso_tmap_primary.heic",
    "step 6c: a tmap primary falls back through the altr group to the SDR base (FD17)",
)];

#[test]
fn every_fixture_decodes_or_fails_as_expected_json_says() {
    let files = expected();
    assert!(files.len() >= 44, "only {} fixtures listed", files.len());
    for (name, e) in &files {
        if let Some((_, step)) = PENDING.iter().find(|(n, _)| n == name) {
            assert!(
                read(name).is_err(),
                "{name} decodes now: take it off PENDING ({step})"
            );
            eprintln!("pending: {name} ({step})");
            continue;
        }
        let kind = e["kind"].as_str().unwrap();
        let expect = e["expect"].as_str().unwrap();
        let result = read(name);
        match expect {
            "error" => assert!(result.is_err(), "{name}: decoded, expected an error"),
            "too_large" => assert!(
                matches!(result, Err(DecodeError::TooLarge(_))),
                "{name}: {result:?}, expected TooLarge"
            ),
            // Decoding the over-cap sequence's first frame is a 4096² still; its truncation is
            // step 6d's.
            "decodes" | "truncated" => {
                let img = result.unwrap_or_else(|err| panic!("{name}: {err}"));
                let size = |k: &str| e[k].as_u64().unwrap_or_else(|| panic!("{name}: no {k}"));
                let (w, h) = (size("width"), size("height"));
                assert_eq!(
                    (img.width as u64, img.height as u64),
                    (w, h),
                    "{name}: decoded size"
                );
                if let Some(corners) = e.get("corners") {
                    let tolerance = if kind == "gainmap" { 1.0 } else { 0.2 };
                    check_corners(name, &img, corners, tolerance);
                }
                if kind == "still" && e["bit_depth"].as_u64() != Some(8) {
                    assert_eq!(
                        Some(img.source.bit_depth as u64),
                        e["bit_depth"].as_u64(),
                        "{name}: bit depth"
                    );
                }
            }
            other => panic!("{name}: unknown expectation {other}"),
        }
    }
}

#[test]
fn a_sequence_only_file_shows_its_first_frame() {
    let files = expected();
    let e = &files["seq_only.avif"];
    let img = read("seq_only.avif").expect("decodes");
    let first = &e["frames"][0];
    let p = pixel(&img, 32, 32);
    for c in 0..3 {
        let want = first[c].as_f64().unwrap() as f32 / 255.0;
        assert!(
            (p[c] - want).abs() < 12.0 / 255.0,
            "channel {c}: {} vs {want}",
            p[c]
        );
    }
}

#[test]
fn premultiplied_alpha_comes_back_straight() {
    let files = expected();
    let img = read("premultiplied_alpha.heic").expect("decodes");
    for patch in files["premultiplied_alpha.heic"]["patches"]
        .as_array()
        .unwrap()
    {
        let x = patch["x"].as_u64().unwrap() as usize + 16;
        let p = pixel(&img, x, 16);
        let alpha = patch["alpha"].as_f64().unwrap() as f32 / 255.0;
        assert!(
            (p[3] - alpha).abs() < 1.5 / 255.0,
            "alpha {} vs {alpha}",
            p[3]
        );
        for c in 0..3 {
            let want = patch["straight8"][c].as_f64().unwrap() as f32 / 255.0;
            assert!(
                (p[c] - want).abs() < 6.0 / 255.0,
                "patch at alpha {alpha}: channel {c} is {}, expected {want}",
                p[c]
            );
        }
    }
}
