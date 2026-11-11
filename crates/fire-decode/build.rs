//! Build script for fire-decode: the crate's version, from `product.json` at the repository's
//! root, the one place it is set (as fire and 3D Review keep theirs). It becomes
//! `fire_decode::VERSION`; `scripts/release.sh` names the release tag after the same field.
//!
//! The file is outside this package, which is fine for every way the apps take the crate: a git
//! dependency checks out the whole repository, and a `[patch]` points at a checkout of it.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let path = Path::new(&manifest).join("../../product.json");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "fire-decode reads its version from {}, which it couldn't read: {e}",
            path.display()
        )
    });
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
    let version = json
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{} has no string field \"version\"", path.display()));
    assert!(
        is_release_version(version),
        "product.json's version must be MAJOR.MINOR.PATCH (it names the tag v{version}), not \
         {version:?}"
    );
    println!("cargo:rustc-env=FIRE_DECODE_VERSION={version}");
}

/// Three dot-separated numbers without leading zeros, as a release tag carries them.
fn is_release_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (*p == "0" || !p.starts_with('0'))
        })
}
