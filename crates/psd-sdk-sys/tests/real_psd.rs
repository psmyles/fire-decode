//! Decode a real PSD: 3D Review's tracked `assets/test_textures/T_Sides_D.psd`, from a 3D Review
//! checkout beside this repository (`../3d-review-rs`), or the file `FIRE_DECODE_REAL_PSD` names.
//! Ported from 3D Review's `crates/psd/tests/real_psd.rs` (FD3).
//!
//! The synthetic suite covers the validation and the failure paths; this is the check that the
//! vendored psd_sdk built and decodes a file Photoshop wrote. It skips itself when the file is
//! absent, and `FIRE_DECODE_REQUIRE_FIXTURES=1` turns that skip into a failure, so a run that
//! decides whether the tree is good cannot pass by finding nothing to do.

use std::path::PathBuf;

use psd_sdk_sys::{Limits, decode_psd};

fn fixture() -> Option<PathBuf> {
    let path = std::env::var_os("FIRE_DECODE_REAL_PSD")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../3d-review-rs/assets/test_textures/T_Sides_D.psd")
        });
    path.exists().then_some(path)
}

#[test]
fn decodes_the_merged_composite() {
    let Some(path) = fixture() else {
        let reason = "no real PSD: neither FIRE_DECODE_REAL_PSD nor a 3D Review checkout beside \
                      this one";
        assert!(
            !std::env::var_os("FIRE_DECODE_REQUIRE_FIXTURES").is_some_and(|value| value != "0"),
            "FIRE_DECODE_REQUIRE_FIXTURES is set, so this run may not skip: {reason}"
        );
        eprintln!("skipping: {reason}");
        return;
    };
    let bytes = std::fs::read(&path).expect("read the PSD");
    let limits = Limits {
        max_dim: 30000,
        max_output_bytes: 1 << 30,
        max_planar_bytes: 2 << 30,
        max_channels: 56,
    };
    let image = decode_psd(&bytes, &limits, 4).expect("decode the merged composite");
    assert!(image.width > 0 && image.height > 0);
    assert!(
        matches!(image.bits_per_channel, 8 | 16 | 32),
        "bits per channel came back as {}",
        image.bits_per_channel
    );
    assert_eq!(
        image.rgba.len(),
        image.width as usize * image.height as usize * 4 * usize::from(image.bits_per_channel / 8),
        "the buffer is exactly width * height * RGBA at the document's depth"
    );
    // A decode that silently produced nothing would still satisfy the length check.
    assert!(
        image.rgba.iter().any(|&b| b != 0),
        "every byte is zero: the merged image was never written"
    );
}
