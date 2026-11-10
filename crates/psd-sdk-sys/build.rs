//! Build script for psd-sdk-sys.
//!
//! - The Rust side of the C-ABI `wrapper.h` is committed (`src/bindings.rs`), so nothing here
//!   needs bindgen or libclang (FD3).
//! - cc compiles the vendored psd_sdk C++ plus `wrapper.cpp` into a static lib: MSVC on
//!   Windows (`cl.exe`, not clang, so the STL constraint does not apply there), clang on
//!   macOS. psd_sdk is already clang-aware (`PsdPch.h` sets `PSD_USE_CLANG`, `PsdPlatform.h`
//!   gates `<windows.h>` on `_WIN32`), and `wrapper.cpp` reads through its own
//!   `MemoryFile : psd::File`, so the per-platform `NativeFile` is never instantiated and
//!   every platform file can be excluded from the build.

use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let wrapper_h = crate_dir.join("wrapper.h");
    let wrapper_cpp = crate_dir.join("wrapper.cpp");
    let psd_dir = crate_dir.join("vendor").join("Psd");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let windows = target_os == "windows";

    println!("cargo:rerun-if-changed={}", wrapper_h.display());
    println!("cargo:rerun-if-changed={}", wrapper_cpp.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor");

    // --- cc: compile vendored psd_sdk + wrapper.cpp -----------------------------
    if !psd_dir.join("Psd.h").exists() {
        panic!(
            "psd_sdk source missing at {} — vendor it (see vendor/VENDOR.txt)",
            psd_dir.display()
        );
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(&psd_dir)
        .file(&wrapper_cpp)
        .warnings(false); // third-party code; don't fail/noise on its warnings

    // MSVC C++17 + exceptions. cc follows the target's CRT: /MT with crt-static (FD20), else /MD.
    build.flag_if_supported("/std:c++17");
    build.flag_if_supported("/EHsc");
    // clang/gcc C++17 (macOS and any other non-MSVC toolchain).
    build.flag_if_supported("-std=c++17");

    for entry in std::fs::read_dir(&psd_dir).expect("read vendor/Psd") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("cpp") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // Exclude the platform `NativeFile` sources. `_Linux.cpp` is POSIX aio and `_Mac.mm`
        // is Objective-C++ (never matched here — not a .cpp); `PsdNativeFile.cpp` is the Win32
        // `CreateFileW` + overlapped-IO one, which only compiles under MSVC. None are reachable:
        // `wrapper.cpp` reads through its own `MemoryFile`.
        if name.ends_with("_Linux.cpp") || name.ends_with("_Mac.cpp") {
            continue;
        }
        if !windows && name == "PsdNativeFile.cpp" {
            continue;
        }
        build.file(&path);
    }

    // cc emits the C++ stdlib link flag itself for a `cpp(true)` build (`c++` on macOS), so
    // there is nothing to name here.
    build.compile("fire_psd");
}
