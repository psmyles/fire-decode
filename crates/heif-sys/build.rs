//! Build script for heif-sys.
//!
//! - `cc` compiles `wrapper.c` against the vendored libheif headers into a static lib.
//! - The prebuilt vendored static libs (libheif + libde265 + dav1d) are linked.
//! - The Rust side of `wrapper.h` is committed (`src/bindings.rs`), so nothing here needs bindgen
//!   or libclang (FD16).
//!
//! The vendored artifacts are per-target: one directory per target, each holding the headers
//! *and* the libs from the same vcpkg run. See `vendor/VENDOR.txt` for the recipe.
//!
//! Only the targets the apps ship are supported (FD16): x64 Windows MSVC with the static CRT
//! (FD20), and arm64 macOS. Anything else gets a named `compile_error!` through
//! `$OUT_DIR/target_check.rs`, rather than a failure deep in the compiler or the linker.

use std::env;
use std::path::PathBuf;

/// A vendored target: which `vendor/` subdirectory holds it, and how it links.
struct Vendor {
    /// Subdirectory of `vendor/`, holding `include/` and `lib/`.
    dir: &'static str,
    /// Static libs to link, by cargo link name (so `heif` is `heif.lib` / `libheif.a`).
    libs: &'static [&'static str],
    /// The C++ runtime to name explicitly, where the object format cannot request it itself.
    cpp_runtime: Option<&'static str>,
}

/// The vendored stack for this target, or why there is none.
fn vendor_for(os: &str, arch: &str, env: &str, crt_static: bool) -> Result<Vendor, String> {
    match (os, arch, env) {
        // MSVC objects embed `/DEFAULTLIB` directives for the C++ runtime that libheif and
        // libde265 need, so it links itself and must not be named here. The libraries are built
        // /MT, so the crate must be too: a mixed-CRT link fails or misbehaves (FD20).
        ("windows", "x86_64", "msvc") if crt_static => Ok(Vendor {
            dir: "x64-windows",
            libs: &["heif", "libde265", "dav1d"],
            cpp_runtime: None,
        }),
        ("windows", "x86_64", "msvc") => Err(
            "heif-sys needs the static CRT on Windows: build with `-C target-feature=+crt-static` \
             (fire-decode plan FD20), set in the app's .cargo/config.toml"
                .into(),
        ),
        // Mach-O has no equivalent of `/DEFAULTLIB`, so the C++ runtime is named explicitly or
        // libheif's and libde265's C++ symbols go unresolved at link time.
        ("macos", "aarch64", _) => Ok(Vendor {
            dir: "arm64-macos",
            libs: &["heif", "de265", "dav1d"],
            cpp_runtime: Some("c++"),
        }),
        _ => Err(format!(
            "heif-sys has no vendored libheif stack for {arch}-{os}-{env}: only x86_64 Windows \
             MSVC and arm64 macOS are vendored (see crates/heif-sys/vendor/VENDOR.txt)"
        )),
    }
}

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let wrapper_c = crate_dir.join("wrapper.c");
    let var = |name: &str| env::var(name).unwrap_or_default();
    let crt_static = var("CARGO_CFG_TARGET_FEATURE").split(',').any(|f| f == "crt-static");

    println!("cargo:rerun-if-changed={}", crate_dir.join("wrapper.h").display());
    println!("cargo:rerun-if-changed={}", wrapper_c.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor");

    let target = vendor_for(
        &var("CARGO_CFG_TARGET_OS"),
        &var("CARGO_CFG_TARGET_ARCH"),
        &var("CARGO_CFG_TARGET_ENV"),
        crt_static,
    );
    let check = match &target {
        Ok(_) => String::new(),
        Err(why) => format!("compile_error!({why:?});\n"),
    };
    std::fs::write(out_dir.join("target_check.rs"), check).expect("failed to write target_check.rs");
    let Ok(vendor_target) = target else {
        return;
    };

    let vendor = crate_dir.join("vendor").join(vendor_target.dir);
    let inc = vendor.join("include");
    let lib = vendor.join("lib");
    if !inc.join("libheif").join("heif.h").exists() {
        panic!(
            "libheif headers missing at {} — vendor the static build (see vendor/VENDOR.txt)",
            inc.display()
        );
    }

    // --- cc: compile wrapper.c against the vendored libheif headers --------------
    cc::Build::new()
        .file(&wrapper_c)
        .include(&inc)
        .warnings(false)
        .compile("fire_heif");

    // --- link the vendored static decoder stack ---------------------------------
    println!("cargo:rustc-link-search=native={}", lib.display());
    // Dependents before dependencies, for linkers that care; MSVC resolves across the set.
    for name in vendor_target.libs {
        println!("cargo:rustc-link-lib=static={name}");
    }
    if let Some(runtime) = vendor_target.cpp_runtime {
        println!("cargo:rustc-link-lib=dylib={runtime}");
    }
}
