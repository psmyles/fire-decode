# fire-decode - Work Plan

**Status, 10 November 2026:** the crate lives here now (step 1's Mac part), the repository is public,
and steps 2a to 5 are done on a Mac. fire, 3D Review and bite each build on it on a `fire-decode`
branch, which merges when they move to the first tag. Everything that needs Windows is collected in
"Needs Windows", kept current as the Mac work goes, to be done in one stretch afterwards.

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
   - **macOS baselines - done** (10 November 2026), in [`baselines.md`](baselines.md):
     - fire's TTFP per format (13 files, HEIC / AVIF / EXR / PSD included);
     - 3D Review's binary sizes and startup gate;
     - bite's cold and warm imports by format, with `magick` process counts, through the new
       [`bench/bite-import`](../bench/bite-import/src/main.rs).

     The corpus is bite's `test_images` plus derived AVIF / EXR files and two PSDs. The baseline
     binaries and derived files are kept in the untracked `local/`.
   - **On this Mac TTFP can't resolve 2%.** Identical binaries differ by up to ~5% at 12 launches.
     So the macOS test is that no format gets slower, per the user. 3D Review's Windows baselines are
     done (its `docs/PLAN.md`).

3. **Step 1, the parts a Mac can do - done** (10 November 2026), merged and pushed.
   - **What came in:**
     - fire's three crates with their history (35 commits), the vendored binaries dropped;
     - a standalone workspace: edition 2024, Rust 1.99.1, 3D Review's lints, no default features;
     - the macOS HEIF stack re-vendored, from vcpkg `e529425f2f`: libheif 1.23.7, libde265 1.1.3,
       dav1d 1.5.4, `minos 11.0`, no plugin loading, no encoders;
     - committed bindings asserted on both sides, with no bindgen;
     - `links = "fire_psd"`, and heif-sys's target guards;
     - licence texts and `THIRD-PARTY-NOTICES.md` with the relink section;
     - the macOS CI job.
   - **The macOS half of the gate passed.** fire on the extracted crate passes its 194 tests, every
     format's TTFP is within the noise of the baseline, an AVIF decodes (dav1d linked), and
     `minos` is 11.0 ([`baselines.md`](baselines.md)).
   - **Still Windows:** see "Needs Windows".


4. **Step 2a, the PSD merge (FD3) - done on the Mac** (10 November 2026).
   - **Patches:** both sets, with the two RLE fixes reconciled. A malformed run is refused, with the
     rest of the plane still zero-filled. `vendor/fire-decode.patch` is the whole diff against
     upstream, and `vendor/VENDOR.txt` lists each hunk's origin.
   - **The wrapper (ABI v2):**
     - 3D Review's staged decode, borrowed bytes and checks;
     - fire's colour modes, document depth and integer path;
     - limits and the thread count from Rust;
     - the threaded interleave extended to the converted modes;
     - the composite's ICC profile from Rust's `composite_icc`, never psd_sdk's resources parser.
   - **The Rust side:** 3D Review's `DocHandle<'bytes>`, the output allocated fallibly
     (`OutOfMemory`), and specific errors.
   - **Tests:** 3D Review's 17 synthetic cases ported to psd-sdk-sys (plus a threads case) and its
     real-file test (`T_Sides_D.psd` from the 3D Review checkout), beside fire's 20.
   - **Two fire tests changed meaning.** A malformed RLE run is now refused. A document with two
     ICC profiles now decodes, taking the first, because the parser whose assert it reached is no
     longer called.
   - **psd_sdk stays at `f514495`.** Upstream's later PSB commits don't touch the merged path
     (VENDOR.txt).
   - **The macOS gate passed:** fire's tests, both suites, and TTFP on four PSDs up to 8192², none
     slower ([`baselines.md`](baselines.md)).

5. **Step 2b, the HEIF wrapper v2 (FD16) - done on the Mac** (10 November 2026).
   - **`wrapper.cpp` (ABI v2):**
     - `catch (...)` at every entry point;
     - `heif_init` once per process through `std::call_once`, `heif_deinit` never;
     - libheif's security limits set from the caller's (pixels, total memory, tiles, sequence
       frames, profile size);
     - the signalled size checked against the limits before decoding;
     - thread counts split by tiling (FD8);
     - progress and cancel hooks;
     - `ignore_transformations` and nclx passthrough;
     - one copy, straight into a buffer Rust allocates, with bit replication and un-premultiply.
   - **Already in the ABI for later steps:** sequence tracks (first visual track, `decode_next`,
     timescale, repetitions; the edit list ignored), auxiliary images by URN, and raw item data.
   - **fire-decode:** decodes through it. A sequence-only file shows its first frame. Errors map to
     `TooLarge` / `Malformed` / `Ffi`. Progress waits for step 3's `Sync` callback.
   - **The macOS gate passed:**
     - every malformed and oversized fixture is an error, never an abort, the oversized ones
       `TooLarge`;
     - the depth, grid, portrait, mirror, sequence-only and premultiplied fixtures are correct
       (`tests/heif_fixtures.rs`; the `tmap`-primary file waits for 6c);
     - busy threads stay within the budget;
     - TTFP is no slower anywhere, and the 48 MP HEIC is 21% faster ([`baselines.md`](baselines.md)).
   - **Measured:** libheif's default rendering of PQ / HLG reinterprets the signal, and it doesn't
     convert Display P3 nclx either. Both are recorded in FD17 and FD5.
   - **Settled after it (10 November):** `Sdr` converts PQ / HLG to SDR itself in step 6b (203
     cd/m² at white, highlights clipped, sRGB-encoded), so bite's previews of those files will
     differ from ImageMagick's.

6. **Step 3, the API changes - done on the Mac** (10 November 2026). The signatures as built are in
   `fire-decode-plan.md` §4, with what changed on the way.
   - **The crate:**
     - the four named setups with their limits;
     - `output`, `fit` with the area filter, `frames`, `surfaces`, `stored_mips`, `max_threads`;
     - `source` (`SourceInfo`), the public `Format`, `sniff`, both extension lists and `probe()`;
     - `DecodeError::Unsupported` / `Cancelled` / `Panicked`, and the panic guard with
       `allow-abort`;
     - `DecodeCtl`, wired into libheif;
     - the `icc` and `raw` features, and Display P3 / BT.2020 nclx converted in `viewer()`.
   - **The thread budget** is the cores less two, never below `min(cores, 8)`. Passes over memory
     take at most 8 threads, and computation (colour, the area filter, libheif) takes the rest.
     Each choice is measured in [`baselines.md`](baselines.md).
   - **fire's `fire-decode` branch** builds on it. The in-tree crates are gone and
     `Cargo.toml` points at this repository, patched to this checkout by an untracked
     `~/Dev/.cargo/config.toml`. On the branch:
     - the viewer is on `viewer()`;
     - fire-shellext is on `fire_thumbs::decode_options()`;
     - fire-cli is on `inspect()`, with no per-worker thread share;
     - the installer test has moved in from the crate.
   - **The macOS gate passed:**
     - 217 tests here, and fire's 197;
     - extensions under every CI set;
     - `probe()` equals `decode()` over the fixtures and 188 corpus images;
     - fire-shellext's setup still draws every face of a cubemap;
     - fire-cli reports a rotated JPEG's stored size;
     - no format slower in-process (HEIC 14.5% faster) or in fire's TTFP (48 MP HEIC 7% faster).

7. **Step 4, 3D Review on `texture()` - done on the Mac** (10 November 2026), on 3D Review's
   `fire-decode` branch, which merges with the first tag. Its `docs/PLAN.md` (B.3) has the change
   and [`baselines.md`](baselines.md) the gate:
   - startup +1.6% against a 10 ms budget;
   - pixels identical but for what the plan names;
   - HEIC / AVIF textures in Quick Look and the viewer's renderer;
   - the over-limit upload intact.

   For it, the crate gained `in_decode()`, so a panic hook can tell a decode's panic, its helper
   threads' included, from any other (FD7).

8. **Step 5, bite on `thumbnail()` and `probe()` - done on the Mac** (10 November 2026), on
   bite's `fire-decode` branch (`01ee7ff`), which merges with the first tag. bite has no work plan
   of its own: the commit message has the change, the plan's FD8-FD10, FD12 and FD19 have it as
   built, and [`baselines.md`](baselines.md) has the gate:
   - **Cold import:** faster for every routed format, by 63-92%. JPG, still on ImageMagick, is
     unchanged.
   - **Warm re-import:** no `magick` process for any crate format.
   - **`probe()` against `identify`:** the same size on all 115 files `identify` can read.
   - **HEIC orientation:** identical to ImageMagick's.
   - **Thumbnails:** the same picture, ICC kept, alpha where it was.
   - **Over 30000 px:** still thumbnailed, through ImageMagick.
   - **Cache size:** unchanged. Thumbnails stay lossy at quality 85, through libwebp: lossless
     ones were three to seven times larger, and the lossy fallback was chosen (FD12).
   - **Two findings:**
     - Concurrent decodes must each be capped to their share of the cores: a lease is greedy and
       held for the whole decode (FD8). bite caps them.
     - ImageMagick un-blends a white matte from any PSD with a 4th merged channel, which is wrong
       for a flat mask texture. Whether Photoshop's own transparent documents need it is open
       (plan §6, PSD).
   - **Decision for later:** a fairer lease in the crate (FD8).

9. **Next on the Mac, in the plan's order.** Each works on a branch through the same `[patch]`; its
   gate's Windows half joins "Needs Windows":
   - **step 6** (fire only, any time after 3): the tonemap's negatives (6a), PQ / HLG to `Linear`
     and to SDR (6b), gain maps (6c), sequences with HEIF progress and cancel in the loading bar
     (6d);
   - **step 7:** the `mips` feature.

The gates are in `fire-decode-plan.md` §5. On a Mac only their macOS halves can pass, so record each
gate as "macOS passed; Windows pending".

## Needs Windows (later)

The Windows stretch, in order. Each item names what it unblocks.

1. **Step 0's Windows baselines.** Take them before anything changes fire or bite on Windows.
   - **fire TTFP** with `scripts/ttfp.ps1`, from fire `710b3f6`, over the same 13 files as the macOS
     baseline (`docs/baselines.md`; the derived AVIF / EXR files are in the Mac's `local/corpus/`).
     Keep a copy of the baseline `fire.exe` for the A/B later.
   - **bite:** cold and warm import by format with `bench/bite-import`, and `BITE_MAGICK` set to the
     bundled `magick.exe`. Also `bite-gui.exe`'s and `bite.exe`'s import tables (FD20 expects only
     `VCRUNTIME140`).
   - 3D Review's Windows baselines are done (its `docs/PLAN.md`).
2. **Re-vendor the Windows HEIF stack** (FD16, FD20), from the same vcpkg baseline as macOS
   (`crates/heif-sys/vendor/VENDOR.txt` has the recipe):
   - `vcpkg install "libheif[core,dav1d,libde265]:x64-windows-fire-decode"` with
     `--overlay-triplets=crates/heif-sys/vendor/triplets --binarysource=clear`. The triplet is
     committed: static libraries, /MT, release only, no plugin loading.
   - Strip with `llvm-objcopy --strip-debug`, and copy `include/libheif` and the three libraries to
     `crates/heif-sys/vendor/x64-windows/`. Check the library names against `build.rs`, which links
     `heif`, `libde265` and `dav1d`.
   - Run VENDOR.txt's checks: codec references, no `LoadLibrary`, `/DEFAULTLIB:LIBCMT` (not
     `MSVCRT`), no encoders.
   - Update VENDOR.txt and `THIRD-PARTY-NOTICES.md` with the Windows versions, and commit the
     libraries alone in one commit.
3. **CI's Windows job** (FD13): fmt, clippy and tests for the four sets the macOS job runs (core,
   psd, psd + heif, psd + heif + icc + raw) on `windows-latest`, with `crt-static`. Its `.cargo/config.toml` or `RUSTFLAGS` must not reach the macOS job's
   "heif without crt-static is a named error" check.
4. **fire on the static CRT** (FD20): `-C target-feature=+crt-static` for `x86_64-pc-windows-msvc` in
   fire's `.cargo/config.toml`. Check that nothing prebuilt is `/MD`, and that `fire.exe`,
   `fire-cli.exe` and `fire_shellext.dll` import no `MSVCP140`, `VCRUNTIME140*` or
   `api-ms-win-crt-*`.
5. **Step 1's Windows gate**, with fire built against this repository (as on the Mac,
   `docs/baselines.md`):
   - fire's tests pass;
   - TTFP within 2% per format against item 1's baseline, HEIC / AVIF included;
   - fire-shellext thumbnails a HEIC;
   - an AVIF decodes (dav1d linked).
6. **The first release:** tag `v0.1.0`. Its GitHub release carries the upstream source tarballs of
   libheif 1.23.7, libde265 1.1.3 and dav1d 1.5.4, plus the triplets (FD14). The tarballs can be
   fetched anywhere; the tag waits for item 2.
7. **fire moves to the tag**, by merging its `fire-decode` branch, which already drops the three
   in-tree crates and carries the installer test (FD2, FD9). Then:
   - `fire-decode = { git = "https://github.com/psmyles/fire-decode", tag = "v0.1.0" }` in place of
     `branch = "main"`;
   - fire's CI follows: no `-p fire-decode` in the decoder-free job, no vendored-artifact checks or
     LLVM install in the native one (the crate brings its libraries, and bindgen is gone), and a
     toolchain of at least 1.99, the crate's `rust-version`. fire's `stable` here is 1.98.1, so the
     Mac builds used `cargo +1.99.1`;
   - fire's notices point at this repository's tree at the tag, with libheif / libde265 as
     LGPL-3.0-or-later, not `-only` (FD14).

8. **Step 2a's Windows half:** psd-sdk-sys's tests and fire-decode's `tests/psd.rs` (CI's Windows
   job covers both), and fire's TTFP on the four PSDs in `docs/baselines.md` against item 1's
   baseline binary.

9. **Step 2b's Windows half:**
   - `tests/heif_fixtures.rs` and fire-decode's HEIF tests (CI's Windows job);
   - fire's TTFP on the three iPhone HEICs, their AVIFs and `grid_iphone.heic`, against item 1's
     baseline;
   - the thread check on `IMG_2135.HEIC`. There's no `ps -M` on Windows: count threads with
     Process Explorer or `Get-Process`;
   - fire-shellext thumbnails a HEIC through ABI v2.

10. **Step 3's Windows half** (fire's `fire-decode` branch, or `main` after item 7):
    - **fire-shellext compiles:** its decode call changed and only builds on Windows. It thumbnails
      a cubemap DDS with every face, and a HEIC, in Explorer;
    - **fire's TTFP** per format against item 1's baseline. The budget's size depends on the
      machine's cores (the cores less two, at least `min(cores, 8)`), so check the HEICs especially;
    - **the per-format bench:**
      - `bench/decode-formats` against step 2b, built in a worktree of `34c48f8` with
        `--features old-api`, over fire's validation set;
      - no format more than 2% slower;
    - **`tests/probe.rs` with `FIRE_DECODE_CORPUS`** pointing at fire's validation set;
    - **fire-cli's tests**, the rotated JPEG included, and Asset Check over CrimeCrew's textures
      with the same results as before, apart from rotated JPEGs now reporting their stored size.
11. **Step 4's Windows half**: 3D Review's `docs/PLAN.md`, "Needs Windows". Its startup gate and
    import tables, HEIC / AVIF in the viewer and Explorer, and Asset Check reading a
    `review3d-cli` panic's exit code 101 as the tool failing.
12. **Step 5's Windows half** (bite's `fire-decode` branch; it needs item 2's libraries):
    - **It builds:** `bite-gui.exe` and `bite.exe` with `crt-static`. Their import tables show no
      `MSVCP140`, `VCRUNTIME140*` or `api-ms-win-crt-*` (step 0 expected `VCRUNTIME140`, before
      the static CRT).
    - **bite's tests** pass, the ImageMagick ones included, against the bundled `magick.exe`.
    - **Cold and warm import by format:** `bench/bite-import` with `BITE_MAGICK` set to the
      bundled `magick.exe`, against item 1's baseline. Every routed format faster cold, and no
      `magick` process on a warm re-import of PNG / TGA / PSD / EXR / HEIC / AVIF.
    - **The installer** (`cargo xtask package-windows`) ships the new `THIRD_PARTY_LICENSES`. A
      HEIC folder imports through the HEIC chip in the installed app.
    - **A transparent PSD saved by Photoshop:** a layered document with no Background layer and
      some soft-edged transparency, saved with Maximize Compatibility. Compare Photoshop's view
      with fire-decode's merged image and ImageMagick's. Does the merged colour carry a white
      matte that the crate should divide out (plan §6, PSD)?

## References

- [`fire-decode-plan.md`](../fire-decode-plan.md).
- `docs/asset-check-plan.md` in SandboxVR/exp-content-rules: the Asset Check design.
- `docs/PLAN.md` in psmyles/fire, psmyles/3d-review, psmyles/asset-rules, psmyles/launcher and
  SandboxVR/exp-content-rules.
