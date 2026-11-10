# Step 0 baselines

The numbers later steps are compared against (`fire-decode-plan.md` §5, step 0). Nothing has moved
yet: each app decodes as it does today.

- **macOS half:** recorded 10 November 2026, below.
- **Windows half:**
  - 3D Review's is in its own `docs/PLAN.md` (4 November 2026).
  - fire's and bite's are still to do (`docs/PLAN.md`, "Needs Windows").

## The machine and the builds

- **Machine:** MacBook Pro (Mac15,11), Apple M3 Max, 14 cores (10 performance, 4 efficiency),
  36 GB, macOS 27.2, driving a 5K display.
- **Builds:** release builds of fire `710b3f6`, 3D Review `746f64b` and bite `b511984`. The commits
  after those heads changed only docs.
- **Baseline binaries:** copies are kept in the untracked `local/bin/`, so later runs can A/B
  against the same files:
  - `fire-710b3f6`;
  - `3d-review-746f64b` and `libreview_quicklook-746f64b.dylib`;
  - `bite-b511984`.

## The corpus

Most files are in bite's local `test_images` folder (untracked). The rest are noted in the table.
The derived files live in the untracked `local/corpus/`.

| File | Format | Size | Source |
| --- | --- | --- | --- |
| `28-4928x3264.jpg` | JPEG | 4928×3264 | `test_images` |
| `58-1280x853.jpg` | JPEG | 1280×853 | `test_images` |
| `T_Clock_D.jpg` | JPEG | 2896×1944 | `test_images` |
| `T_CliffRock_01_N.png` | PNG | 4096×4096 | `test_images` |
| `T_DeadTree_03_N.png` | PNG | 1024×1024 | `test_images` |
| `Explosion02HD_5x5.tga` | TGA | 2048×2048 | `test_images` |
| `FireBall02_8x8.tga` | TGA | 1024×1024 | `test_images` |
| `IMG_2131.HEIC` | HEIC, 512 px grid | 4032×3024 | `test_images`, iPhone |
| `IMG_2135.HEIC` | HEIC, 640×896 grid | 5712×4284 | `test_images`, iPhone (48 MP) |
| `IMG_2139.HEIC` | HEIC, `irot` 270 | 3024×4032 shown | `test_images`, iPhone |
| `IMG_2131.avif`, `IMG_2135.avif`, `IMG_2139.avif` | AVIF, 8-bit | as their HEICs, shown upright | derived: `heif-dec` to PNG, `heif-enc -A -q 75` (libheif 1.23.6, aom 3.15.2) |
| `T_CliffRock_01_N.exr`, `T_CliffRock_02_N.exr` | EXR, half RGBA, ZIP16 | 4096×4096 | derived: `bench/png-to-exr` from the PNGs of the same name |
| `T_Sides_D.psd` | PSD, 8-bit | 1024×1024 | 3D Review's `assets/test_textures` |
| `T_Noise_Burshed_01_M.psd` | PSD, 8-bit | 2048×2048 | CrimeCrew-Research's `Assets/Content/Shared_Assets/Textures`; bite only |

## fire: time to first pixel

**Method:** fire's `scripts/ttfp.sh`, 12 launches each of two copies of the same binary, interleaved:
```sh
scripts/ttfp.sh -A local/bin/fire-710b3f6 -B ../fire/target/release/fire -N 12 <files>
```

| File | Median (ms, 24 launches) | sd | Gap between the two identical runs |
| --- | --- | --- | --- |
| `28-4928x3264.jpg` | 222.8 | 10.5 | 0.3% |
| `58-1280x853.jpg` | 222.0 | 12.3 | 2.3% |
| `T_Clock_D.jpg` | 225.2 | 13.8 | 1.9% |
| `T_CliffRock_01_N.png` | 242.3 | 13.4 | 2.2% |
| `T_DeadTree_03_N.png` | 226.3 | 12.6 | 4.8% |
| `Explosion02HD_5x5.tga` | 228.5 | 12.1 | 2.3% |
| `FireBall02_8x8.tga` | 224.2 | 13.6 | 1.2% |
| `IMG_2131.HEIC` | 223.7 | 13.1 | 3.1% |
| `IMG_2135.HEIC` | 337.3 | 10.0 | 3.5% |
| `IMG_2139.HEIC` | 226.2 | 16.3 | 0.6% |
| `IMG_2131.avif` | 235.6 | 16.0 | 0.5% |
| `T_CliffRock_01_N.exr` | 258.5 | 14.0 | 0.7% |
| `T_Sides_D.psd` | 223.2 | 12.9 | 1.5% |

**Reading it:**
- **Startup dominates.** Most files land at about 222-242 ms whatever their size. Decode shows
  through only for the 48 MP HEIC, the EXR and the 4K PNG.
- **On this Mac the harness can't resolve 2%.** Identical binaries differ by up to 4.8% in median
  at 12 launches each.
- **The macOS test is that no format gets slower beyond that noise.** Per the user, 10 November
  2026: TTFP was always measured on Windows, and what matters is that it doesn't get worse than the
  older version. A 2% call here needs many more launches.

The raw launches are in `local/ttfp-macos-710b3f6.csv`.

## 3D Review: sizes and the startup gate

| Binary | Bytes | `minos` |
| --- | --- | --- |
| `3d-review` | 19,661,536 | 11.0 |
| `libreview_quicklook.dylib` (`release-shell`) | 7,647,360 | 11.0 |

`3d-review` links only system frameworks and `libc++`.

**The startup gate:** 3D Review's `scripts/gate.sh` in one-build mode, on
`assets/test_models/SM_Wall_Break_4x3m.fbx`.
- **The model:** 1128 triangles, 1 clip. The Windows baseline's `Bot.fbx` isn't on this Mac, so
  this baseline uses a different model.
- **Conditions:** 1920×1280, 2× MSAA, GTAO on, clip playing. 5 measured runs after a warm-up.

| Measure | Median | Min | Max |
| --- | --- | --- | --- |
| startup (ms) | 257.8 | 245.8 | 261.8 |
| frame (ms) | 6.482 | 6.366 | 6.565 |
| frame p95 (ms) | 7.089 | 6.894 | 7.448 |
| cpu/frame (ms) | 6.437 | 6.274 | 6.520 |
| footprint (MB) | 496.0 | 493.0 | 497.0 |
| audit (ms) | 164.9 | 164.6 | 165.8 |

To compare a later build:
```sh
scripts/gate.sh -A ../fire-decode/local/bin/3d-review-746f64b -B target/release/3d-review \
    -f assets/test_models/SM_Wall_Break_4x3m.fbx
```

## bite: cold and warm import

**Method:** [`bench/bite-import`](../bench/bite-import/src/main.rs) does a folder import the way
bite-gui's `add_paths` does:
- `load_batch`, 16 files at a time, at 256 px;
- then `work::decode_many` over the cached thumbnails.

It runs two passes over one cache folder:
- **cold:** the folder starts empty;
- **warm:** a fresh `ThumbnailCache`, as every GUI import builds, over the now-full folder. Neither
  of bite's own benchmarks measures this (plan §6).

Each format has its own folder, and the median of 3 repetitions is reported. `magick` is bite's
bundled 7.1.2-34 (`BITE_MAGICK=../bite/resources/mac/magick/bin/magick`). The harness's cache
folder is separate from the GUI's.

| Format | Files | Cold (ms) | `magick` processes, cold | Warm (ms) | `magick` processes, warm |
| --- | --- | --- | --- | --- | --- |
| PNG | 5 | 547 | 1 | 3 | 0 |
| TGA | 8 | 389 | 1 | 3 | 0 |
| JPG | 23 | 614 | 3 | 5 | 0 |
| PSD | 2 | 188 | 1 | 120 | 2 |
| EXR | 2 | 834 | 1 | 799 | 2 |
| HEIC | 3 | 621 | 1 | 604 | 3 |
| AVIF | 3 | 709 | 1 | 694 | 3 |

**Reading it:**
- **A warm re-import of PSD, EXR, HEIC or AVIF is almost as slow as a cold one.** Every cache hit
  in those formats runs one `magick identify` just for the size, one after another. That is what
  `probe()` removes (FD19, step 5's gate).
- **PNG, TGA and JPG** are read by `header.rs`, so their warm imports start no process.
- **Cold:** one `magick` command per 8 misses.

Every file was thumbnailed and decoded, with no failures. The raw passes are in
`local/bite-import-macos-b511984.jsonl`.

To compare a later build, rerun it the same way:
```sh
cp ../bite/Cargo.lock bench/bite-import/
cargo build --release --manifest-path bench/bite-import/Cargo.toml
BITE_MAGICK=../bite/resources/mac/magick/bin/magick \
    bench/bite-import/target/release/bite-import-baseline <format folder> <scratch cache folder> 256
```

## Step 1 on macOS: fire on the extracted crate

10 November 2026. fire `710b3f6` was built against this repository's `step-1` branch (`ee4d447`).
It ran in a scratch clone whose workspace drops its in-tree copies of the three crates and points
`fire-decode` at this checkout. Rust 1.99.1, macOS libheif 1.23.7 / libde265 1.1.3 / dav1d 1.5.4.

- **fire's tests:** 194 passed, 0 failed (`cargo test --workspace`; the in-tree decode crates are
  out of that workspace).
- **The binary:** `minos 11.0`, 10,416,944 bytes (134 KB more than the baseline).
- **TTFP:** `scripts/ttfp.sh -A local/bin/fire-710b3f6 -B local/bin/fire-step1-ee4d447 -N 12`,
  same 13 files. Every format decodes, HEIC and AVIF included:

| File | Baseline (ms) | Step 1 (ms) | Change |
| --- | --- | --- | --- |
| `28-4928x3264.jpg` | 228.6 | 227.0 | -0.7% |
| `58-1280x853.jpg` | 221.2 | 224.5 | +1.5% |
| `T_Clock_D.jpg` | 223.3 | 228.4 | +2.3% |
| `T_CliffRock_01_N.png` | 241.3 | 241.2 | -0.1% |
| `T_DeadTree_03_N.png` | 216.1 | 214.6 | -0.7% |
| `Explosion02HD_5x5.tga` | 214.0 | 217.4 | +1.6% |
| `FireBall02_8x8.tga` | 216.1 | 211.7 | -2.1% |
| `IMG_2131.HEIC` | 214.8 | 210.8 | -1.9% |
| `IMG_2135.HEIC` | 339.2 | 336.8 | -0.7% |
| `IMG_2139.HEIC` | 215.4 | 211.4 | -1.8% |
| `IMG_2131.avif` | 217.0 | 207.5 | -4.3% |
| `T_CliffRock_01_N.exr` | 221.4 | 214.9 | -2.9% |
| `T_Sides_D.psd` | 207.3 | 205.7 | -0.8% |

No format is slower beyond the harness's noise (up to 4.8% between identical binaries, above).
The JPEG, PNG and TGA paths are unchanged code. So step 1's macOS gate passes; its Windows half
waits for the Windows libraries. The raw launches are in `local/ttfp-macos-step1.csv`.

## Step 2a on macOS: the merged PSD bridge

10 November 2026. fire `710b3f6` built against this repository with the merged psd-sdk-sys (FD3),
in the same scratch clone as step 1:
- **fire's tests:** 194 passed.
- **The suites:** fire-decode's `tests/psd.rs` 20, and psd-sdk-sys's 18 ported from 3D Review plus
  its real-file test.
- **TTFP:** `scripts/ttfp.sh -A local/bin/fire-710b3f6 -B local/bin/fire-step2a -N 12`, on the two
  real PSDs and two made with ImageMagick for size (`local/corpus/`):

| File | Size | Baseline (ms) | Step 2a (ms) | Change |
| --- | --- | --- | --- | --- |
| `T_Sides_D.psd` (3D Review) | 1024×1024 RGB 8-bit | 215.6 | 213.0 | -1.2% |
| `T_Noise_Burshed_01_M.psd` (CrimeCrew) | 2048×2048 RGBA 8-bit | 215.9 | 217.1 | +0.6% |
| `rgb16_4096.psd` | 4096×4096 RGB 16-bit | 217.7 | 213.6 | -1.9% |
| `big_rgba8_8192.psd` | 8192×8192 RGBA 8-bit | 323.5 | 318.3 | -1.6% |

The two made files:
- `magick T_CliffRock_02_N.png -depth 16 rgb16_4096.psd`;
- `magick T_CliffRock_01_N.png -alpha set -channel A -fx "0.5+0.5*sin(i/64)" +channel -resize
  8192x8192 big_rgba8_8192.psd`.

No PSD is slower. The raw launches are in `local/ttfp-macos-step2a.csv`.

## Decoder updates on macOS

10 November 2026: exr 1.74.0 → 1.74.3, lcms2 6.1.1 → 6.2.0 (lcms2-sys 4.0.6 → 4.0.7), zune-core
0.5.1 → 0.5.3, bcdec_rs 0.2.0 → 0.2.1, flate2 1.1.9 → 1.1.11, and their compatible dependencies
([`decoders.md`](decoders.md)). fire was built on them, with steps 1 and 2a, in the same scratch
clone. Its tests passed (194), and so did all of this workspace's (198).

TTFP against the baseline binary, 12 launches each, all 17 files: every format within -2.7% to
+3.1%. The two at +3.1% (`T_Clock_D.jpg`, `FireBall02_8x8.tga`) were re-run with 30 launches each,
alongside `28-4928x3264.jpg` as a control:

| File | Baseline (ms) | Updated (ms) | Change |
| --- | --- | --- | --- |
| `T_Clock_D.jpg` | 212.9 | 213.8 | +0.4% |
| `FireBall02_8x8.tga` | 210.4 | 209.1 | -0.6% |
| `28-4928x3264.jpg` | 211.5 | 211.2 | -0.1% |

Noise, then: no format is slower. The raw launches are in `local/ttfp-macos-deps.csv` and
`local/ttfp-macos-deps-n30.csv`.

## Step 2b on macOS: HEIF wrapper v2

10 November 2026. fire `710b3f6` built against this repository with heif-sys's ABI v2 (FD16),
in the same scratch clone:
- **fire's tests:** 194 passed.
- **This workspace:** 199 tests. `tests/heif_fixtures.rs` checks every `fixtures/heif` file against
  `expected.json`; one waits for step 6c.

**Threads (FD8).** The 48 MP iPhone grid (`IMG_2135.HEIC`, 9×5 tiles) and its AVIF were decoded
three times at each budget, sampling the process's thread count with `ps -M` and its CPU time with
`ps -o cputime`:

| Budget | HEIC: threads | HEIC: cores busy | HEIC: ms | AVIF: threads | AVIF: ms |
| --- | --- | --- | --- | --- | --- |
| libheif's default | 8 | 3.3 | 208 | 14 | 111 |
| 1 | 2 | 1.2 | 652 | 0 | 243 |
| 2 | 4 | 2.0 | 347 | 2 | 146 |
| 4 | 8 | 3.3 | 205 | 4 | 105 |
| 8 | 16 | 5.1 | 134 | 8 | 106 |

- **AVIF:** a single image, decoded on dav1d's threads: exactly the budget. Unbudgeted, dav1d took
  all 14 cores.
- **The HEIC grid:** N tile threads, each with a libde265 decoder that starts one worker thread of
  its own. So 2N threads, of which one per pair waits. Busy cores stay within the budget (the
  sampler accounts for about 0.2).

**TTFP**, 12 launches each, against `local/bin/fire-710b3f6`:

| File | Baseline (ms) | Step 2b (ms) | Change |
| --- | --- | --- | --- |
| `IMG_2131.HEIC` | 187.9 | 189.2 | +0.7% |
| `IMG_2135.HEIC` (48 MP) | 329.1 | 258.4 | -21.5% |
| `IMG_2139.HEIC` (portrait) | 210.1 | 194.4 | -7.5% |
| `IMG_2131.avif` | 193.6 | 192.4 | -0.6% |
| `IMG_2135.avif` | 238.7 | 234.2 | -1.9% |
| `IMG_2139.avif` | 186.9 | 191.7 | +2.6% |
| `fixtures/heif/grid_iphone.heic` | 182.3 | 189.6 | +4.0% (mean +0.6%, sd 9.6) |

The 48 MP grid is faster because the budget gives it 8 tile threads where libheif's default is 4,
and because a copy is gone. No format is slower beyond noise. The raw launches are in
`local/ttfp-macos-step2b.csv`.

## Step 3 on macOS: the API changes

10 November 2026. This repository at `1e93600`; fire's `fire-decode` branch (`f45b210`), which
drops the in-tree crates and builds on this checkout through an untracked `[patch]` in
`~/Dev/.cargo/config.toml`. Rust 1.99.1.

- **This workspace:** 217 tests with every feature, 172 with none; clippy clean on all four CI sets.
- **fire's tests:** 197 passed, with and without features: the 194, plus the installer test moved
  in from the crate, fire-cli's rotated JPEG (it reports its stored 16×8, not the upright 8×16) and
  fire-thumbs' cubemap thumbnail (decoded with fire-shellext's setup, all six faces drawn).
- **The binary:** `minos 11.0`, 10,501,328 bytes (115 KB less than step 2b's).
- **`probe()` against `decode()` with `inspect()`** (`tests/probe.rs`): equal size and format on
  every format the test builds, on every HEIF fixture that decodes, and on 188 corpus images (bite's
  `test_images`, `local/corpus`, 3D Review's `assets`, CrimeCrew's `Assets`, fire's `assets`): 128
  PNG, 23 JPEG, 17 TGA, 6 Radiance HDR, 4 PSD, 3 HEIC, 3 AVIF, 2 EXR, 2 ICO.

**Per-format decode** (`bench/decode-formats`, in process, fire's setup): the median of 15 decodes
per file, the median of three interleaved rounds, against step 2b (`34c48f8`). The raw files are
`local/bench-step2b-r*.tsv` and `local/bench-step3-r*.tsv`.

| Format | Files | Step 2b (ms, sum) | Step 3 (ms, sum) | Change |
| --- | --- | --- | --- | --- |
| JPEG | 23 | 692.3 | 694.0 | +0.2% |
| PNG | 5 | 275.4 | 275.2 | -0.1% |
| TGA | 8 | 13.7 | 13.6 | -0.9% |
| PSD | 3 | 195.9 | 196.2 | +0.2% |
| OpenEXR | 2 | 68.9 | 66.9 | -2.9% |
| HEIC | 3 | 297.7 | 254.4 | -14.5% |
| AVIF | 3 | 257.6 | 249.6 | -3.1% |

No format is more than 2% slower (FD13's rule for a tag). HEIC and AVIF gain from the budget:
libheif and the colour transform their iPhone profiles need now lease the free cores, where step
2b's budget stopped at 8.

**Threads (FD8).** Three things changed on the way, each measured:
- A pass leasing 14 threads on a 4-8 MiB buffer was 0.06-0.25 ms slower than 8 (up to 9% of a
  1 ms TGA): the spawns cost more than the memory-bound pass saved. Passes over memory now take at
  most 8, and nothing under 1 MiB is split.
- Capping every pass at 8 also capped the colour transform, and HEIC's gain fell from 20% to 14%.
  Work bound by computation (lcms2, the area filter, libheif) takes every free core again.
- Leasing all 14 cores slowed fire's TTFP on a 12 MP HEIC by 2% (30 launches each), though the
  decode was faster: fire's startup competes for them. With the budget at 8 the gain on a 48 MP
  HEIC went; at 12 (the cores less two) both held. The budget is now the cores less two, never
  below `min(cores, 8)`.

**TTFP**, 12 launches each, against `local/bin/fire-step2b` (`local/ttfp-macos-step3.txt`):

| File | Step 2b (ms) | Step 3 (ms) | Change |
| --- | --- | --- | --- |
| `28-4928x3264.jpg` | 193.7 | 190.6 | -1.6% |
| `58-1280x853.jpg` | 190.2 | 191.3 | +0.6% |
| `T_Clock_D.jpg` | 195.3 | 197.3 | +1.0% |
| `T_CliffRock_01_N.png` | 218.4 | 221.8 | +1.6% |
| `T_DeadTree_03_N.png` | 196.1 | 195.5 | -0.3% |
| `Explosion02HD_5x5.tga` | 201.9 | 198.6 | -1.6% |
| `FireBall02_8x8.tga` | 195.2 | 198.9 | +1.9% |
| `IMG_2131.HEIC` | 199.3 | 200.3 | +0.5% |
| `IMG_2135.HEIC` (48 MP) | 258.6 | 240.2 | -7.1% |
| `IMG_2139.HEIC` (portrait) | 198.8 | 201.8 | +1.5% |
| `IMG_2131.avif` | 194.0 | 202.4 | +4.3% (30 launches: +0.2%) |
| `IMG_2135.avif` | 237.6 | 233.5 | -1.7% |
| `IMG_2139.avif` | 199.1 | 198.3 | -0.4% |
| `T_CliffRock_01_N.exr` | 223.6 | 222.5 | -0.5% |
| `T_Sides_D.psd` | 193.6 | 194.6 | +0.5% |
| `rgb16_4096.psd` | 204.9 | 204.8 | -0.0% |
| `big_rgba8_8192.psd` | 301.0 | 300.4 | -0.2% |

Rerun at 30 launches (`local/ttfp-macos-step3-n30.txt`, `-ab.txt`), the 12 MP HEICs came out
between -5.0% and +2.0% and the TGA between -1.9% and +1.6%, with an A/A run of step 2b against
itself 5.4% apart on that TGA in the same session (`-aa.txt`). No format is slower beyond the
harness's noise, and the 48 MP HEIC is 7% faster. So step 3's macOS gate passes; its Windows half
is in `docs/PLAN.md`, "Needs Windows".

## Step 4 on macOS: 3D Review on fire-decode

10 November 2026. 3D Review's `fire-decode` branch (`5abe8c3`) against this repository at
`13454b7`, through the same untracked `[patch]` as fire. Rust 1.99.1. The details of the change are
in 3D Review's `docs/PLAN.md`, B.3.

**Tests:** all of 3D Review's pass but `a_classic_material_imports_as_a_dielectric`, which fails on
its `main` too: no local fixture declares a ReflectionFactor. Clippy is clean.

**Sizes and imports** against step 0 (`local/bin/3d-review-746f64b`):

| Binary | Step 0 (bytes) | Step 4 (bytes) | Imports | `minos` |
| --- | --- | --- | --- | --- |
| `3d-review` | 19,661,536 | 27,247,728 (+7.6 MB) | unchanged | 11.0 |
| `libreview_quicklook.dylib` (`release-shell`) | 7,647,360 | 13,195,376 (+5.5 MB) | unchanged | 11.0 |

The growth is the HEIF stack, JPEG XL, OpenEXR and the other formats 3D Review didn't read.

**The startup gate**, `scripts/gate.sh -n 15` against `746f64b` on `SM_Wall_Break_4x3m.fbx`
(`local/gate-3d-review-macos-step4-n15.txt`):

| Measure | Step 0 | Step 4 | Change | Budget |
| --- | --- | --- | --- | --- |
| startup (ms) | 279.2 | 283.7 | +4.5 (+1.6%) | +10.0 |
| frame (ms) | 3.413 | 3.394 | -0.5% | +0.3 |
| footprint (MB) | 496.0 | 497.0 | +0.2% | +5% |
| audit (ms) | 165.5 | 108.1 | -34.7% | - |

Passed. The default 5 runs read startup +20.9 ms (`-step4.txt`); a build with `panic = "abort"`
read 10 ms *slower* than the unwinding one in the same session, so neither unwinding nor the
crate's six static initializers is the cost, and 15 runs a side settled it. The fixture has no
textures, so no decode is on the measured path.

**Pixel comparison**, every texture through 3D Review's old decoder (`main`) and the new one, over
bite's `test_images`, `local/corpus`, 3D Review's and CrimeCrew's assets, its six IBL `.hdr`
sources and hand-made 16 / 32-bit PSDs: 111 files (`local/step4/pixel-compare.txt`).
- **97 byte-identical:** JPEG, PNG (8 and 16-bit), TGA, 8-bit PSD, and 16-bit PSDs whose samples
  are multiples of 257.
- **8 decode where they failed:** 3 HEIC, 3 AVIF, 2 OpenEXR.
- **A 32-bit PSD's colour reading is byte-identical** to the old bridge's; its data reading is new.
- **Radiance HDR:** the data reading equals the old output (clamped, no curve); the colour reading
  is now sRGB-encoded (up to 73 codes brighter), so a `.hdr` in a Base Color slot is shown right.
- **A 16-bit PSD with arbitrary samples:** 1,522 of 8,192 bytes 1 code apart, rounded where the
  old bridge truncated.

**HEIC and AVIF textures:** FBX planes made in Blender with an iPhone HEIC, and an AVIF, embedded as
their base colour draw the photo through the Quick Look dylib (`scripts/quicklook-check.sh`:
thumbnail and preview) and through `review-snapshot`, the viewer's renderer
(`local/step4/*.png`).

**Over the GPU limit:** an FBX embedding a 20000×400 texture draws its gradient in
`review-snapshot`: `texture()` doesn't shrink it, and the upload starts from the first mip within
Metal's 16384.

## Step 5 on macOS: bite on fire-decode

10 November 2026. bite's `fire-decode` branch (`01ee7ff`) against this repository at `00b2247`,
through the same untracked `[patch]`. Rust 1.99.1, bite's bundled ImageMagick 7.1.2-34. The change
is in the messages of the branch's two commits (`5aa3905`, then `01ee7ff`, which made the
thumbnails lossy); the plan's FD9, FD10, FD12 and FD19 have it as built.

**Tests:** bite's 373 pass and clippy is clean. bite-imagemagick's ImageMagick tests used to skip
on a Mac (their helper tried the Windows `magick.exe` first); they run now, and two are new: a
broken file is retried through ImageMagick for both routes, and a 30001×2 PNG, which fire-decode
refuses, still gets its thumbnail.

**Cold and warm import**, `bench/bite-import` built against bite `b511984` (a worktree) and against
the branch, run alternately, 5 repetitions, median (`local/bite-import-macos-step5.jsonl`; the
driver is `local/step5/run-bench.sh`):

| Format | Files | Cold before (ms) | Cold after (ms) | Change | `magick`, cold | Warm before (ms) | Warm after (ms) | `magick`, warm |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| PNG | 5 | 542 | 160 | -70% | 1 → 0 | 3 | 2 | 0 → 0 |
| TGA | 8 | 390 | 32 | -92% | 1 → 0 | 3 | 2 | 0 → 0 |
| JPG | 23 | 612 | 614 | +0% | 3 → 3 | 5 | 3 | 0 → 0 |
| PSD | 2 | 188 | 24 | -87% | 1 → 0 | 121 | 2 | 2 → 0 |
| EXR | 2 | 843 | 96 | -89% | 1 → 0 | 802 | 2 | 2 → 0 |
| HEIC | 3 | 623 | 231 | -63% | 1 → 0 | 601 | 1 | 3 → 0 |
| AVIF | 3 | 710 | 138 | -81% | 1 → 0 | 707 | 1 | 3 → 0 |

Every routed format is faster cold, and no warm re-import of one starts a process. JPG stays on
ImageMagick. All 46 files at once: 3,094 → 910 ms cold at 256 px, 7,991 → 3,225 ms at 1024 px.
With lossless thumbnails (`5aa3905`) TGA took 14 ms and PSD 11: lossy encoding costs more on
those, and nothing measurable on the others.

**Concurrent decodes need a cap.** The first run had HEIC 5% *slower* (654 ms). A lease takes the
threads that are free when a decode starts and keeps them to the end, so the first of three
concurrent HEICs took nearly every core and the other two ran on one each. Three iPhone HEICs
through `thumbnail(256)`, by how they were spread:

| Spread | HEIC (ms) | AVIF (ms) | PNG (ms) |
| --- | --- | --- | --- |
| one at a time | 243 | 238 | 300 |
| three at once, uncapped | 637 | 246 | 144 |
| three at once, 4 threads each | 219 | 120 | 144 |

bite now caps each of its concurrent decodes at the cores divided by the decodes (`max_threads`).

**`probe()` against `identify`:** the bite corpus, bite's `test_images` and the HEIF fixtures, 129
files (`local/step5/probe-vs-identify.tsv`). All 115 that `identify` reads have the same size. The
other 14 are files `identify` can't read: malformed fixtures, the size bombs, and the `tmap`-primary
file. For those `probe()` reports the header's size, the decode fails, and bite leaves the file out
as before.

**HEIC orientation:** the portrait iPhone photo and the `irot` / `imir` / gain-map portrait fixtures
come out identical to ImageMagick's thumbnails (MAE 0 on the fixtures; a mirrored copy differs by
10%). ImageMagick applies `irot` / `imir`, as FD5 expected.

**Thumbnails, old against new**, over the 23 routed files, at 256 px (thumbnail sizes match on every
file):
- **The same picture** (`local/step5/thumbs-old-vs-new.png`, made with the lossless files). The
  mean difference is 0.2-1.4% for TGA, HEIC, AVIF and `T_Sides_D.psd`, and 1.7-2.2% for the
  normal maps, PNG and EXR: the area filter against ImageMagick's resize, then quality 85 on both.
- **The encoder is ImageMagick's:** libwebp's simple API at 85 writes byte-for-byte what
  `magick -quality 85` writes for the same pixels (checked on three thumbnails).
- **ICC:** the HEICs', AVIFs' and `T_Noise_Burshed_01_M.psd`'s profiles are embedded, as before.
- **Alpha:** present on exactly the files that had it before.
- **EXR** shows clamped with no curve in both: ImageMagick reads it that way too, so EXR is routed
  with no sRGB option.
- **One deliberate difference, `T_Noise_Burshed_01_M.psd`** (11%;
  `local/step5/psd-noise-im-vs-crate-vs-alpha.png`). It is a flat document whose 4th channel is a
  mask. ImageMagick treats any 4th channel of the merged image as transparency over a white matte
  and divides the matte out, which crushes this texture's colour nearly to black. fire-decode
  keeps the colour as stored, which is right for this file. It is also right for ImageMagick's own
  RGBA PSD (`local/corpus/big_rgba8_8192.psd`, layer count -1), which ImageMagick un-blends to 0.19
  where its source is 0.50. A transparent document saved by Photoshop is untested (Needs Windows).

**Cache size:** unchanged. The 46-file corpus's cache is 824 KB at 256 px (836 before) and 8,028
KB at 1024 px (8,028 before). The branch's first commit wrote lossless files, which were three to
seven times larger per file:

| Format | 256 px q85 / lossless (KiB) | 1024 px q85 / lossless (KiB) |
| --- | --- | --- |
| HEIC, AVIF | 10 / 63 | 106 / 780 |
| PNG | 18 / 101 | 250 / 1,430 |
| EXR | 29 / 169 | 423 / 2,514 |
| PSD | 29 / 108 | 364 / 1,469 |
| TGA | 31 / 64 | 222 / 693 |

So the thumbnails are lossy at quality 85, as they were (FD12's fallback, chosen on 10 November).

**The chip grid:** `bite-gui --capture` has a new `input-folder` scene. HEIC sits beside AVIF, off
by default, and the eleventh chip starts a third row (`local/step5/input-folder-capture.png`).

**Not checked here:**
- a Linux build of the CLI: `heif` is off there, but psd_sdk needs a Linux C++ compiler, which
  this Mac doesn't have;
- `bite-gui.exe`'s and `bite.exe`'s import tables with `crt-static`, the Windows timings and the
  installer (Needs Windows).

## Steps 6a and 6b on macOS: tone curves and PQ / HLG

10 November 2026. This repository at `8449e9b`; fire's `fire-decode` branch at `28a892d` (6a is
`1a88bbb`). Rust 1.99.1.

**6a, the tone curves:** fire's 199 tests pass with the two new ones. Regenerating the shaders
from the unchanged `shader.glsl` reproduces the committed files byte for byte, so the regenerated
diff beyond the two clamps is SPIR-V renumbering.

**6b, fixtures:** `tests/heif_fixtures.rs` against `fixtures/heif/expected.json`, every patch
centre in both modes. The four lossless fixtures pass at 0.1% (the test allows 0.2%); the lossy
`pq_ycbcr.avif` is off by up to 0.9% (allowed 2%). Reference white (PQ code 594, HLG 767) is at
least 0.999 of white in `Sdr`. The crate's suite passes under all four CI feature sets (173 /
196 / 215 / 226), with clippy clean.

**6b, nothing else moved:** `texture()` and `thumbnail(256)` output, hashed (format, size, pixels,
alpha flag, ICC length) by builds of `f99ff88` and `8449e9b`, over 137 files: the bite corpus,
`local/corpus`, bite's `test_images`, every HEIF fixture and 3D Review's test textures. 132 are
identical, errors included; the five that differ are the PQ / HLG fixtures.

**6b, SDR HEIC TTFP**, `scripts/ttfp.sh -A local/bin/fire-step3 -B local/bin/fire-step6b -N 12`
(`local/ttfp-macos-step6b.csv`):

| File | Step 3 (ms) | Step 6b (ms) | Change |
| --- | --- | --- | --- |
| `IMG_2131.HEIC` | 212.5 | 220.0 | +3.5% (20 launches: -2.1%) |
| `IMG_2135.HEIC` | 259.0 | 258.7 | -0.1% |
| `IMG_2139.HEIC` | 212.5 | 208.2 | -2.0% |
| `grid_iphone.heic` | 215.3 | 209.7 | -2.6% |

Within the noise (identical binaries differ by up to ~5% here). An SDR HEIC's only new work is one
check of its nclx kind.

**6b in fire:** `pq_lossless.avif` and `hlg_lossless.heic` open in the step 6b build and reach
their first frame as `Rgba16Float` (the TTFP harness), through the viewer's exposure and tone
curve. bite and 3D Review still build against the crate unchanged.

## Step 6c on macOS: gain maps

10 November 2026. This repository at `fada526`; fire's `fire-decode` branch at `1ccce64`.

**Fixtures** (`tests/heif_fixtures.rs`, `Hdr::Linear`): all seven gain-map fixtures render
within 3% of ImageIO's HDR values on their patches (ISO `b` 3.40, Apple `a` 4.00 and `b` 1.66),
at their displayed sizes, corners in place. `gainmap_both.heic` renders its ISO map. The
`tmap`-primary file now decodes: its base under `Sdr`, identical to `gainmap_iso.heic`'s.

**Real iPhone photos against ImageIO** (`gainmaps.swift decode`, ImageIO's own HDR rendering,
compared in 16 px blocks of luminance; `local/step6/gainmap-vs-imageio.txt`):

| Photo | Size | Crate decode | Mean difference | Worst block | Peak, ImageIO / crate |
| --- | --- | --- | --- | --- | --- |
| `IMG_2131.HEIC` | 4032×3024 | 99 ms | 0.26% | 1.2% | 1.63 / 1.63 |
| `IMG_2135.HEIC` | 5712×4284 | 218 ms | 0.21% | 1.4% | 2.01 / 2.01 |
| `IMG_2139.HEIC` (portrait) | 3024×4032 | 119 ms | 0.26% | 1.4% | 3.14 / 3.15 |

Each carries both maps; the ISO one is used. Apple's MakerNote formula gives 2.0470, 2.6287 and
3.1499 against the XMP headroom's 2.0469, 2.6285 and 3.1498.

**Nothing else moved:** `texture()` / `thumbnail(256)` output over the same 137 files as 6b
is identical on 131; the six that differ are the five PQ / HLG fixtures and the `tmap`-primary
file, which was an error before.

**SDR HEIC TTFP** (`local/ttfp-macos-step6c.csv`, against step 3, 12 launches): `IMG_2131`
-2.4%, `IMG_2135` -1.4%, `IMG_2139` +2.9%, `grid_iphone` +1.6%. Within noise; the `Sdr` path's
new work is one parse of the `meta` box.

**fire:** 202 tests (200 without `heif`), clippy clean.
