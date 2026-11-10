# fire-decode - Work Plan

**Status, 10 November 2026:** the crate lives here now (step 1's Mac part), the repository is public,
and work continues on a Mac with steps 2a, 2b and 3. Everything that needs Windows is collected in
"Needs Windows", kept current as the Mac work goes, to be done in one stretch afterwards. fire,
3D Review and bite still decode with their own copies until they move to a tag.

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

6. **Step 3, the API changes** (setups including `inspect()`, `output` including `Rgba8OrFloat`,
   `source`, `limits`, `hdr`, `fit`, `frames`, `surfaces`, `stored_mips`, `probe()`, thread budget,
   panic guard, `DecodeCtl`). Platform-neutral. fire uses them on a branch, through a
   `[patch]` pointing at this checkout, in a `.cargo/config.toml` in the folder that holds both
   checkouts. fire's own `.cargo/config.toml` is committed, so it isn't the place.

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
3. **CI's Windows job** (FD13): fmt, clippy and tests for core, psd and psd + heif on `windows-latest`,
   with `crt-static`. Its `.cargo/config.toml` or `RUSTFLAGS` must not reach the macOS job's
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
7. **fire moves to the tag:**
   - `fire-decode = { git = "https://github.com/psmyles/fire-decode", tag = "v0.1.0" }`, and fire's
     three in-tree crates removed;
   - the installer-association test moves into a fire crate, checking `all_extensions()` once step 3
     adds it (FD2, FD9);
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

The Windows half of step 3 is added below when step 3 is done on the Mac.

## References

- [`fire-decode-plan.md`](../fire-decode-plan.md).
- `docs/asset-check-plan.md` in SandboxVR/exp-content-rules: the Asset Check design.
- `docs/PLAN.md` in psmyles/fire, psmyles/3d-review, psmyles/asset-rules, psmyles/launcher and
  SandboxVR/exp-content-rules.
