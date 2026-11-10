# fire-decode - Work Plan

**Status, 10 November 2026:** this repository holds designs only. Asset Check is done, so the queue
below starts now. The crate itself still lives in fire
(`crates/fire-decode`, `crates/psd-sdk-sys`, `crates/heif-sys`). The work below is ordered for a stretch
of work on a Mac. Items that need Windows are collected at the end.

## What's here

| File | What it is |
| --- | --- |
| [`fire-decode-plan.md`](../fire-decode-plan.md) | The migration design: fire-decode becomes the decoder shared by fire, 3D Review and bite, with HEIF / HEIC / AVIF included. Decisions FD1-FD20; rollout steps 0-7 with gates in §5. fire's `docs/fire-decode-plan.md` is a copy, so keep the two in step |
| [`asset-check-plan.md`](../asset-check-plan.md) | A pointer only. The Asset Check design moved on 5 November 2026 to the rules repo, SandboxVR/exp-content-rules (`docs/asset-check-plan.md`); its history up to then is here |

## Priority

**Asset Check first, then the migration.** Asset Check is done (10 November 2026). Its pieces live in
other repositories; their `docs/PLAN.md` files hold the work:
1. psmyles/asset-rules - the shared rules crate (**done**, `v0.1.0`);
2. psmyles/fire - `fire-cli` and three small fire-decode additions (**done**, in `main`);
3. psmyles/3d-review - the `3d-review-cli` changes (**done**, in `main`);
4. psmyles/launcher - the results table, window `--set` and exit codes (**done**, in `main`);
5. SandboxVR/exp-content-rules, the rules repo - `rules.json`, rules files, hook scripts and the
   Launcher tools (Asset Check step 4, **done**; its `docs/PLAN.md`). CrimeCrew is the pilot project.

fire's fire-decode additions are in fire's `main`, so the extraction (item 3 below) can start from it.

**10 November 2026:** the plan was re-checked against fire `710b3f6`, 3D Review `746f64b` and bite
`b511984`, and five points were settled. They are listed in `fire-decode-plan.md` under "Settled on
10 November". fire's copy was updated to match.

## Queue

1. **Licence files (FD1) - done** (10 November 2026). `LICENSE` is now `LICENSE-MIT`, and
   `LICENSE-APACHE` is the standard Apache-2.0 text, the same file as fire's and 3D Review's
   `licenses/Apache-2.0.txt`.

2. **Step 0 on the Mac.**
   - **Build the HEIF test files - done** (10 November 2026): 44 generated files in
     [`fixtures/heif/`](../fixtures/heif/README.md), all redistributable.
     - They cover every type on step 0's list, plus mirror, premultiplied alpha, an HEVC sequence,
       the animation cap and two grid bombs.
     - `make.py` rebuilds and checks them; `expected.json` is what a decoder must return.
     - The gain maps come from ImageIO, with ImageIO's own HDR decode recorded as the reference.
     - The README lists what building them showed. FD17's transform order was changed to match:
       composite in stored orientation, then apply the base's transforms. Three real iPhone photos
       agree: bite's `test_images/IMG_2131`, `IMG_2135` and `IMG_2139.HEIC`, the last a portrait.
     - **Still to collect, for the local corpus:** an Android ISO-gain-map AVIF and ICC-tagged HEICs.
       They stay out of the repository.
   - **Record macOS baselines:** fire TTFP per format (fire's `scripts/ttfp.sh`), plus 3D Review's
     startup gate and binary sizes. 3D Review's Windows baselines are done (its `docs/PLAN.md`).
   - **bite's baselines:** cold and warm import by format, counting `magick` processes.
     - The corpus needs PSD, EXR and HEIF files. bite's `test_images` holds only JPG, TGA and PNG.
     - Measure the warm import as the GUI does it: a fresh `ThumbnailCache` over a full disk cache.
       Neither of bite's benchmarks does that (plan §6).
     - Clear the thumbnail cache before each run.

3. **Step 1, the parts a Mac can do.** Start only **after fire's Asset Check additions have landed**
   (fire `docs/PLAN.md` A1), so they come over with the history.
   - **Extract** `crates/fire-decode`, `crates/psd-sdk-sys` and `crates/heif-sys` from fire with
     `git filter-repo`, keeping their history but **dropping `crates/heif-sys/vendor/*/lib` and
     `vendor/*/include` from it** (FD16).
     - That takes two passes: one keeping the three paths, then one removing the vendor paths with
       `--invert-paths`.
   - **A standalone workspace:**
     - dependencies no longer inherited from fire's workspace;
     - edition 2024 and the latest stable Rust;
     - lints at 3D Review's level;
     - no default features (FD13).
   - **Re-vendor the macOS HEIF stack** following `crates/heif-sys/vendor/VENDOR.txt`, with these
     changes (FD16):
     - the newest libheif and dav1d in one vcpkg baseline;
     - `arm64-osx` with `VCPKG_OSX_DEPLOYMENT_TARGET=11.0`, because today's libraries say `minos 27.0`;
     - the vcpkg baseline commit and every library version recorded in `VENDOR.txt`, so the Windows
       re-vendor uses the same baseline;
     - checks: libheif references dav1d (`nm -u … | grep dav1d`), no plugin-loading code, and `minos`
       11.0 (`otool -l … | grep -A3 LC_BUILD_VERSION`).
   - **Committed, hand-kept bindings** with size and offset asserts for heif-sys (`wrapper.h`) and
     psd-sdk-sys, and bindgen removed (FD3, FD16).
   - **Target guards in heif-sys's `build.rs`:**
     - `target_env = "msvc"` on Windows;
     - `crt-static` required on Windows;
     - a named `compile_error!` on any other target.
   - **Licence texts** grouped by feature (LGPL-3.0-or-later for libheif and libde265, with
     `LGPL-3.0.txt` and `GPL-3.0.txt`), and the LGPL relink section (FD14).
   - **CI** (FD13): the macOS job now; the Windows job once the Windows libraries exist.
   - **Not yet:**
     - the Windows libraries;
     - the first tag;
     - making the repository public;
     - fire switching to the tag.

     See "Needs Windows".

4. **Step 2a, the PSD merge (FD3).** Cross-platform; develop and test on the Mac.
   - **Patches:** the union of fire's patches and 3D Review's `review.patch` (P1, extended P2), with the
     two RLE fixes reconciled.
   - **Tests:** port 3D Review's malformed-file tests (`crates/psd/tests/synthetic.rs`) and
     `real_psd.rs` alongside fire's `tests/psd.rs`.
   - **ICC:** the composite's ICC profile through `psd_layers.rs`'s Rust `find_icc`, never psd_sdk's
     image-resources parser.
   - **Plus** `links = "fire_psd"`.

5. **Step 2b, the HEIF wrapper v2 (FD16).** Develop and test on the Mac with the re-vendored macOS
   libraries.
   - A C++ wrapper with `catch (...)` at every entry point, and a versioned C ABI;
   - `heif_init` once per process;
   - size guard and libheif security limits passed in from Rust;
   - thread counts and progress / cancel hooks;
   - output into a Rust-allocated buffer;
   - the first frame of sequence-only files.
   - Also measure what libheif's SDR rendering does to PQ / HLG files.

6. **Step 3, the API changes** (setups including `inspect()`, `output` including `Rgba8OrFloat`,
   `source`, `limits`, `hdr`, `fit`, `frames`, `surfaces`, `stored_mips`, `probe()`, thread budget,
   panic guard, `DecodeCtl`). Platform-neutral. fire uses them on a branch, through a
   `[patch]` pointing at this checkout, in a `.cargo/config.toml` in the folder that holds both
   checkouts. fire's own `.cargo/config.toml` is committed, so it isn't the place.

The gates are in `fire-decode-plan.md` §5. On a Mac only their macOS halves can pass, so record each
gate as "macOS passed; Windows pending".

## Needs Windows (later)

- **Re-vendor the Windows HEIF stack:** `x64-windows-static` (/MT) on the same vcpkg baseline,
  debug-stripped (FD16, FD20).
- **The Windows halves** of every gate: TTFP, tests, `fire.exe` / `fire_shellext.dll` import tables
  with no VC++ runtime DLLs.
- **Then** make the repository public, tag `v0.1.0` with the source tarballs on the release (FD14), and
  move fire to the tag (migration step 1's gate).

## References

- [`fire-decode-plan.md`](../fire-decode-plan.md).
- `docs/asset-check-plan.md` in SandboxVR/exp-content-rules: the Asset Check design.
- `docs/PLAN.md` in psmyles/fire, psmyles/3d-review, psmyles/asset-rules, psmyles/launcher and
  SandboxVR/exp-content-rules.
