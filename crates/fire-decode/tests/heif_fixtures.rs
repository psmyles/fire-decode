//! Every file in `fixtures/heif/` through `decode`, against what `expected.json` says it must
//! return (fire-decode plan, step 2b's gate): sizes and orientation, bit depths, grids, the first
//! frame of a sequence-only file, straightened alpha, and an error, never an abort, for every
//! malformed or oversized file.
#![cfg(feature = "heif")]

use std::path::PathBuf;

use fire_decode::{
    DecodeError, DecodeOptions, DecodedImage, Hdr, HdrKind, PixelFormat, Primaries, Sample, decode,
};
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
        fire_decode::PixelFormat::Rgba16Float => {
            let s = |c: usize| {
                let b = &img.pixels[(i + c) * 2..(i + c) * 2 + 2];
                half_to_f32(u16::from_ne_bytes([b[0], b[1]]))
            };
            [s(0), s(1), s(2), s(3)]
        }
        _ => {
            let s = |c: usize| img.pixels[i + c] as f32 / 255.0;
            [s(0), s(1), s(2), s(3)]
        }
    }
}

/// IEEE half to f32, for reading `Rgba16Float` without a dependency.
fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let frac = (h & 0x3ff) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * 2f32.powi(e as i32 - 15),
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

/// The PQ / HLG fixtures, with how far a decoded patch may sit from `expected.json`: the lossless
/// ones keep exact code values, `pq_ycbcr.avif` is lossy YCbCr as such files usually come.
const HDR_FIXTURES: &[(&str, f32)] = &[
    ("pq_lossless.heic", 0.002),
    ("pq_lossless.avif", 0.002),
    ("hlg_lossless.heic", 0.002),
    ("hlg_lossless.avif", 0.002),
    ("pq_ycbcr.avif", 0.02),
];

fn decode_with(name: &str, opts: &DecodeOptions) -> Result<DecodedImage, DecodeError> {
    let bytes = std::fs::read(fixtures().join(name)).expect("read fixture");
    decode(&bytes, name.rsplit('.').next(), opts)
}

/// Each patch's centre against `want`, relative to the larger of the two plus a floor.
fn check_patches(
    name: &str,
    img: &DecodedImage,
    tolerance: f32,
    want: impl Fn(&Value) -> [f32; 3],
) {
    let files = expected();
    for patch in files[name]["patches"].as_array().unwrap() {
        let at = |k: &str| patch[k].as_u64().unwrap() as usize;
        let (x, y) = (at("x") + at("size") / 2, at("y") + at("size") / 2);
        let got = pixel(img, x, y);
        let want = want(patch);
        for c in 0..3 {
            let off = (got[c] - want[c]).abs();
            let allowed = tolerance * got[c].abs().max(want[c].abs()) + tolerance / 10.0;
            assert!(
                off <= allowed,
                "{name}: patch at ({x}, {y}) channel {c} is {}, expected {} (codes {})",
                got[c],
                want[c],
                patch["code10"]
            );
        }
        assert!((got[3] - 1.0).abs() < 1e-3, "{name}: alpha {}", got[3]);
    }
}

fn linear_709(patch: &Value) -> [f32; 3] {
    [0, 1, 2].map(|c| patch["linear_bt709"][c].as_f64().unwrap() as f32)
}

/// Step 6b's gate (FD17): `Linear` holds light in Rec.709, 203 cd/m² = 1.0, colours outside
/// Rec.709 kept, negatives included, as `expected.json` computes it from each patch's codes.
#[test]
fn pq_and_hlg_decode_to_linear_light() {
    let mut opts = DecodeOptions::inspect();
    opts.hdr = Hdr::Linear;
    for &(name, tolerance) in HDR_FIXTURES {
        let img = decode_with(name, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(img.format, PixelFormat::Rgba16Float, "{name}");
        let kind = if name.starts_with("pq") {
            HdrKind::Pq
        } else {
            HdrKind::Hlg
        };
        assert_eq!(img.source.hdr, kind, "{name}");
        assert_eq!(img.source.primaries, Primaries::Bt2020, "{name}");
        assert_eq!(
            img.source.sample,
            Sample::Unorm,
            "{name}: the file is still 10-bit"
        );
        assert_eq!(img.source.bit_depth, 10, "{name}");
        assert!(img.icc.is_none(), "{name}");
        check_patches(name, &img, tolerance, linear_709);
    }
}

/// `Sdr`: the same light clipped at 1.0 and sRGB-encoded, so 203 cd/m² is white.
#[test]
fn pq_and_hlg_decode_to_sdr_with_reference_white_at_white() {
    let srgb = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    for &(name, tolerance) in HDR_FIXTURES {
        let img = decode_with(name, &DecodeOptions::inspect()).unwrap();
        assert_eq!(img.format, PixelFormat::Rgba16Unorm, "{name}");
        check_patches(name, &img, tolerance, |p| linear_709(p).map(srgb));
        // The reference-white patch (third from the right for PQ, sixth for HLG) is white.
        let patches = &expected()[name]["patches"];
        let white = patches
            .as_array()
            .unwrap()
            .iter()
            .find(|p| (p["nits_bt2020"][0].as_f64().unwrap() - 203.0).abs() < 1.0)
            .unwrap_or_else(|| panic!("{name}: no 203 cd/m² patch"));
        let at = |k: &str| white[k].as_u64().unwrap() as usize;
        let got = pixel(&img, at("x") + 16, at("y") + 16);
        let floor = if tolerance > 0.01 { 0.99 } else { 0.999 };
        assert!(
            got[..3].iter().all(|&v| v >= floor),
            "{name}: white is {got:?}"
        );
    }
}

/// The setups narrow the `Sdr` result like any 16-bit image, and `Linear` refuses `Rgba8`.
#[test]
fn hdr_outputs_per_setup() {
    let name = "pq_lossless.heic";
    for opts in [DecodeOptions::texture(), DecodeOptions::thumbnail(64)] {
        let img = decode_with(name, &opts).unwrap();
        assert_eq!(img.format, PixelFormat::Rgba8Unorm);
    }
    let mut texture = DecodeOptions::texture();
    texture.hdr = Hdr::Linear;
    assert_eq!(
        decode_with(name, &texture).unwrap().format,
        PixelFormat::Rgba16Float
    );
    let mut thumbnail = DecodeOptions::thumbnail(64);
    thumbnail.hdr = Hdr::Linear;
    assert!(matches!(
        decode_with(name, &thumbnail),
        Err(DecodeError::Other(_))
    ));
    // An SDR file decodes the same either way.
    let mut viewer = DecodeOptions::viewer();
    let sdr = decode_with("depth10.heic", &viewer).unwrap();
    viewer.hdr = Hdr::Linear;
    let linear = decode_with("depth10.heic", &viewer).unwrap();
    assert_eq!((sdr.format, &sdr.pixels), (linear.format, &linear.pixels));
}

/// `probe()` names the HDR kind before decoding, so an app can choose its `Hdr` (FD17).
#[test]
fn probe_reports_pq_and_hlg() {
    for (name, kind) in [
        ("pq_lossless.heic", HdrKind::Pq),
        ("pq_ycbcr.avif", HdrKind::Pq),
        ("hlg_lossless.avif", HdrKind::Hlg),
        ("depth10.heic", HdrKind::None),
        ("p3_nclx.avif", HdrKind::None),
        ("portrait_irot.heic", HdrKind::None),
    ] {
        let file = std::fs::File::open(fixtures().join(name)).unwrap();
        let p = fire_decode::probe(std::io::BufReader::new(file), name.rsplit('.').next()).unwrap();
        assert_eq!(p.hdr, kind, "{name}");
    }
}
