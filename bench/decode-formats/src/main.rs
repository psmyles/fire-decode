//! Decode every file given (or every file under a directory given) `--reps` times with fire's
//! setup, and print one line per file: name, format, size, best and median milliseconds. Each file
//! is decoded once untimed first, so the first timed run doesn't pay for page faults and lazy
//! library initialisation. Files that don't decode are listed as such.

use std::path::{Path, PathBuf};
use std::time::Instant;

#[cfg(feature = "new-api")]
fn opts() -> fire_decode::DecodeOptions {
    fire_decode::DecodeOptions::viewer()
}

#[cfg(feature = "old-api")]
fn opts() -> fire_decode::DecodeOptions {
    fire_decode::DecodeOptions::default()
}

#[cfg(feature = "new-api")]
fn name(img: &fire_decode::DecodedImage) -> &'static str {
    img.source.name
}

#[cfg(feature = "old-api")]
fn name(img: &fire_decode::DecodedImage) -> &'static str {
    img.source_format
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(p).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            walk(&e, out);
        }
    } else {
        out.push(p.to_path_buf());
    }
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut reps = 9;
    let mut files = Vec::new();
    while let Some(a) = args.next() {
        if a == "--reps" {
            reps = args.next().and_then(|n| n.parse().ok()).expect("--reps N");
        } else {
            walk(Path::new(&a), &mut files);
        }
    }
    let opts = opts();
    println!("file\tformat\tsize\tbest_ms\tmedian_ms");
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let ext = path.extension().and_then(|e| e.to_str());
        let file = path.file_name().unwrap().to_string_lossy();
        let first = match fire_decode::decode(&bytes, ext, &opts) {
            Ok(img) => img,
            Err(_) => continue,
        };
        let (fmt, w, h) = (name(&first), first.width, first.height);
        drop(first);
        let mut times: Vec<f64> = (0..reps)
            .map(|_| {
                let t = Instant::now();
                let img = fire_decode::decode(&bytes, ext, &opts).expect("decoded once");
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                drop(img);
                ms
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!(
            "{file}\t{fmt}\t{w}x{h}\t{:.2}\t{:.2}",
            times[0],
            times[times.len() / 2]
        );
    }
}
