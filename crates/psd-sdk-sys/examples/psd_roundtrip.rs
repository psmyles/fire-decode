//! Round-trip smoke test for the psd_sdk FFI: open a PSD, print its header and the first pixels.
//! Run: `cargo run -p psd-sdk-sys --example psd_roundtrip <file.psd>`
//!
//! The composite's ICC profile is found by fire-decode, not here (FD3), so it isn't printed.

use psd_sdk_sys::{Limits, decode_psd};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: psd_roundtrip <file.psd>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).expect("failed to read PSD");
    // viewer()'s limits (FD5).
    let limits = Limits {
        max_dim: 131072,
        max_output_bytes: 4 << 30,
        max_planar_bytes: 8 << 30,
        max_channels: 56,
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    match decode_psd(&bytes, &limits, threads) {
        Ok(img) => {
            println!(
                "PSD {}x{}  channels={} bits/ch={} mode={}",
                img.width, img.height, img.channels, img.bits_per_channel, img.color_mode
            );
            // `rgba` is in the document's own depth — u8, native-endian u16, or f32 — so the
            // raw bytes are what this smoke test can honestly print.
            let n = img.rgba.len().min(16);
            println!("first RGBA bytes: {:?}", &img.rgba[..n]);
        }
        Err(e) => {
            eprintln!("decode failed: {e}");
            std::process::exit(1);
        }
    }
}
