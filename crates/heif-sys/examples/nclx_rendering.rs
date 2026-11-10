//! What libheif does with an nclx-tagged file when asked for RGB: its default rendering (converted
//! to sRGB) against the source's own colour kept (FD16, FD17). Prints the centre of each 32 px
//! patch of `fixtures/heif`'s PQ / HLG / P3 files, as the sample values the output holds.
//!
//!   cargo run -p heif-sys --example nclx_rendering -- fixtures/heif/pq_lossless.heic

use heif_sys::{Ctl, HeifFile, Limits};

fn main() {
    let limits = Limits {
        max_dim: 32768,
        max_output_bytes: 1 << 30,
        ..Limits::default()
    };
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).expect("read the file");
        let mut file = HeifFile::open(&bytes, &limits).expect("open");
        let id = file.primary_id().expect("a primary image");
        let info = file.item_info(id).expect("info");
        println!(
            "{path}: {}x{} {}-bit nclx {:?}",
            info.width, info.height, info.bit_depth, info.nclx
        );
        for (label, keep) in [("default (sRGB)", false), ("source colour kept", true)] {
            let ctl = Ctl {
                keep_source_color: keep,
                force_16bit: true,
                ..Ctl::default()
            };
            let img = file.decode_item(id, &ctl).expect("decode");
            let at = |x: u32, y: u32| -> [u16; 3] {
                let i = ((y * img.width + x) * 4) as usize * 2;
                let s = |c: usize| {
                    u16::from_ne_bytes([img.pixels[i + c * 2], img.pixels[i + c * 2 + 1]])
                };
                [s(0), s(1), s(2)]
            };
            let row = |y: u32| -> Vec<[u16; 3]> {
                (0..img.width / 32).map(|c| at(c * 32 + 16, y)).collect()
            };
            println!("  {label}, 16-bit samples, row 0: {:?}", row(16));
            if img.height >= 64 {
                println!("  {label}, 16-bit samples, row 1: {:?}", row(48));
            }
        }
    }
}
