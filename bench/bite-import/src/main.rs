//! A folder import the way bite-gui's `add_paths` does one: `load_batch` 16 files at a time, then
//! `work::decode_many` over the cached thumbnails. Run twice over one cache folder:
//!   cold: the folder starts empty;
//!   warm: a fresh `ThumbnailCache` (as each GUI import builds) over the now-full folder.
//! Prints one JSON line per pass: time, files decoded, and magick processes started.
//!   bite-import-baseline <image folder> <cache folder> <size>
use std::{path::PathBuf, sync::{atomic::AtomicBool, Arc}, time::Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (folder, cache_dir, size) = (&args[1], PathBuf::from(&args[2]), args[3].parse::<u32>().unwrap());
    let mut paths: Vec<PathBuf> = std::fs::read_dir(folder).unwrap().flatten().map(|e| e.path())
        .filter(|p| p.is_file()).collect();
    paths.sort();
    let _ = std::fs::remove_dir_all(&cache_dir);
    let jobs = bite_imagemagick::import::default_jobs();
    for pass in ["cold", "warm"] {
        let mut magick = bite_imagemagick::Magick::discover(Arc::new(AtomicBool::new(false)));
        let mut cache = bite_imagemagick::import::ThumbnailCache::new(cache_dir.clone());
        let started = Instant::now();
        let (mut decoded, mut failed) = (0, 0);
        for chunk in paths.chunks(16) {
            let infos = cache.load_batch(&mut magick, chunk, size).expect("thumbnails");
            failed += std::mem::take(&mut cache.failures).len();
            let files: Vec<PathBuf> = infos.iter().map(|i| i.thumbnail.clone()).collect();
            for image in bite_gui::work::decode_many(&files, jobs) {
                match image { Ok(i) if i.width > 0 => decoded += 1, _ => failed += 1 }
            }
        }
        println!("{{\"pass\": \"{pass}\", \"files\": {}, \"decoded\": {decoded}, \"failed\": {failed}, \"ms\": {:.1}, \"magick_processes\": {}}}",
                 paths.len(), started.elapsed().as_secs_f64() * 1000.0, magick.processes());
    }
}
