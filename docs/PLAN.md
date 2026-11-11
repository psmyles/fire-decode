# fire-decode - Work Plan

**Status, 11 November 2026:** the crate lives here, the repository is public, and every step is done
on a Mac. The Windows stretch ran on 11 November: every gate passed but one, 3D Review's texture-load
memory budget, which the user accepted. fire, 3D Review and bite each build on it on a `fire-decode`
branch, which merges when they move to the first tag, `v0.1.0` (released 11 November). Those moves,
and a few hands-on checks, are what remain.

## What's here

| File | What it is |
| --- | --- |
| [`fire-decode-plan.md`](../fire-decode-plan.md) | The migration design: fire-decode becomes the decoder shared by fire, 3D Review and bite, with HEIF / HEIC / AVIF included. Decisions FD1-FD20; rollout steps 0-7 with gates in §5. fire's `docs/fire-decode-plan.md` is a copy, so keep the two in step |
| [`asset-check-plan.md`](../asset-check-plan.md) | A pointer only. The Asset Check design moved on 5 November 2026 to the rules repo, SandboxVR/exp-content-rules (`docs/asset-check-plan.md`); its history up to then is here |
| [`product.json`](../product.json) | The crate's name, version, description and home: the only place they are set, as in fire and 3D Review. `build.rs` reads the version into `fire_decode::VERSION`, and the release tag is `v` + it. The Cargo manifests carry no version |
| [`scripts/release.sh`](../scripts/release.sh) | Tags and publishes a release from `product.json`'s version: checks (main, pushed, CI green, both targets' libraries, a source tarball per vendored library), then the tag and a GitHub release with the tarballs, the triplets and their checksums (FD14). CI fails a tag that isn't `v` + that version |

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

9. **Steps 6a and 6b - done on the Mac** (10 November 2026). The crate's part is in `main`
   (`8449e9b`), fire's on its `fire-decode` branch (`1a88bbb`, `28a892d`); the plan's FD17 has
   them as built and [`baselines.md`](baselines.md) the gates.
   - **6a:** both tone curves, Reinhard and ACES, clamp negatives first, in the viewer's shader
     and in fire-thumbs.
   - **6b:** PQ / HLG HEIF and AVIF decode to light. `Hdr::Linear` is Rec.709 half float at
     203 cd/m² = 1.0; `Sdr` clips at white and sRGB-encodes. `Probe` reports the HDR kind, and
     fire opens PQ / HLG files as HDR.
   - **The gates passed on the Mac:** the fixtures within 0.2% (2% for the lossy one), reference
     white at white, `texture()` / `thumbnail()` unchanged on every other file, SDR HEIC TTFP
     within noise.

10. **Step 6c, gain maps - done on the Mac** (10 November 2026): the crate in `main`
    (`fada526`), fire's HDR toggle on its branch (`1ccce64`). The plan's FD17 has it as built and
    [`baselines.md`](baselines.md) the gate:
    - every gain-map fixture within 3% of ImageIO, the portraits aligned, ISO winning, the
      `tmap` primary showing its base;
    - the three iPhone photos within 0.3% of ImageIO's HDR decode;
    - `Sdr` unchanged (the SDR default), SDR HEIC TTFP within noise.

11. **Step 6d, sequences - done on the Mac** (10 November 2026): the crate in `main`
    (`3f62b4b`), fire on its branch (`94bc48e`). The plan's FD18 has it as built and
    [`baselines.md`](baselines.md) the gate: the infinite loop ends, truncation is flagged,
    progress only rises, and a cancel stops a long sequence. fire plays a sequence's loop count
    and cancels a decode its window has moved on from.

12. **Step 7, the `mips` feature - done on the Mac** (10 November 2026): the crate in `main`
    (`e815b0d`), fire on its branch (`c66ac58`), 3D Review on its branch (`544c20b`, `b653de2`).
    The plan's FD11 has it as built and [`baselines.md`](baselines.md) the gate:
    - **the builder:** `fire_decode::mips`, fire's builder plus 3D Review's raw mode and level
      cap. 8-bit chains are byte-identical to both apps' old builders; the builder is faster on
      every format;
    - **fire's TTFP** within noise on all 13 files;
    - **3D Review** keeps each chain with its decoded image, built on the decode workers for a
      model's textures. The frame that uploads a 15-texture model's textures went from 347 to
      90 ms; frames while they load are unchanged; footprint +3.7%;
    - **the UV-checker bake** keeps its own copy of the filter (`build/mips.rs`), held to the
      crate's by a test; its output is unchanged.

13. **Every step is done on the Mac, and on Windows** (11 November 2026, "Needs Windows"). What
    remains is the apps' merges onto the first tag, `v0.1.0`. One open decision: a fairer lease in the
    crate (FD8, item 8).

The gates are in `fire-decode-plan.md` §5. Each now has both halves recorded in
[`baselines.md`](baselines.md).

## Needs Windows

The Windows stretch was run on 11 November 2026, in order; [`baselines.md`](baselines.md),
"Windows", has every number. Commits are local until pushed.

**Done:**

- (1) **Step 0's Windows baselines:** fire's TTFP over the 13 files (identical binaries within 1% on
  most), bite's cold and warm imports, and the import tables (fire `MSVCP140` and
  `VCRUNTIME140*`; bite only `VCRUNTIME140`). The baseline binaries are in `local/bin/`.
- (2) **The Windows HEIF stack** re-vendored from the same vcpkg baseline, `/MT`, stripped
  (`5c43b53`): libheif 1.23.7, libde265 1.1.3, dav1d 1.5.4. VENDOR.txt's checks pass, and
  VENDOR.txt and the notices record it (`f585dcd`).
- (3) **CI's Windows job** (`f585dcd`): clippy clean on all four sets, tests 175 / 198 / 225 /
  242 / 253 (254 from `b5bc22e`). Green on GitHub from its first run, on `fa1b887`.
- (4) **fire on the static CRT** (fire `837c1d3`, its `fire-decode` branch): `fire.exe`,
  `fire-cli.exe` and `fire_shellext.dll` import no runtime DLL.
- (5) **Step 1's Windows gate:** fire's 199 tests pass; TTFP within 2% or faster on every format
  against step 0 (HEIC up to 23% faster); fire-shellext thumbnails a HEIC and an AVIF (driven
  directly, not yet through Explorer); an AVIF decodes.
- (6) **The first release, `v0.1.0`** (11 November 2026), at `c68d0eb`, by `scripts/release.sh` from
  `product.json`'s version, after CI passed on both targets (the Windows job's first runs on
  GitHub). The [release](https://github.com/psmyles/fire-decode/releases/tag/v0.1.0) carries the
  upstream source tarballs of libheif 1.23.7, libde265 1.1.3 and dav1d 1.5.4 (from vcpkg's
  downloads, SHA-512s as its ports pin them), the two triplets and their SHA-256s (FD14). The
  tag's CI run, with its check that the tag is `v` + `product.json`'s version, passed.
- (8) **Step 2a's Windows half:** the PSD suites (CI's job), and the four PSDs' TTFP within 2%.
- (9) **Step 2b's Windows half:** the HEIF suites; TTFP on the HEICs, AVIFs and `grid_iphone.heic`;
  threads within the budget (`Get-Process`, as `ps -M` on the Mac).
- (10) **Step 3's Windows half:** fire-shellext compiles and thumbnails a HEIC; TTFP per format;
  `bench/decode-formats` against step 2b, no format slower (HEIC -29%, AVIF -14%); `probe()`
  equals `decode()` on 1,937 corpus images, CrimeCrew's included (the test's corpus list now
  takes Windows paths, `7ea7759`); fire-cli's tests, and Asset Check over CrimeCrew's textures
  with byte-identical results.
- (11) **Step 4's Windows half:** 3D Review's import tables (no runtime DLL, OS DLLs unchanged), its
  startup gate (-3.9%) and `check.ps1` (the Mac's three known failures); HEIC / AVIF textures
  through `review-snapshot` and through the Explorer DLL's thumbnail provider; the shell guards
  unchanged; and Asset Check's hook reports a panic's exit code 101 as `tool.failed`.
- (12) **Step 5's Windows half:** bite builds with `crt-static` (no runtime DLL); its 367 tests pass
  against the bundled `magick.exe`; every routed format imports 72-91% faster cold, with no
  `magick` on a warm re-import; the installer ships the new `THIRD_PARTY_LICENSES`.
- (13) **Step 6's Windows half:** the regenerated HLSL compiles; SDR HEIC TTFP within 2% of step 3;
  fire-shellext thumbnails `pq_lossless.avif` as SDR; the PQ, HLG and sequence fixtures open.
- (14) **Step 7's Windows half:** fire's TTFP within 2% of step 6d, the OpenEXRs included; 3D Review's
  startup gate (+1.1%) and upload frame (511 → 67 ms); the UV checker and every render
  byte-identical.

**Decided:** 3D Review's texture-load gate is over its memory budget, private bytes +183 MB
(+11.4%) against 5% on the 15-texture model: the mip chains step 7 keeps with each image, a third
of the decoded bytes (the Mac's +3.7% footprint hid it in unified memory). Accepted by the user on
11 November 2026, as the price of the 87% shorter upload frame.

**Open:**

- **Hands-on checks** (need eyes, Explorer or Photoshop):
  - fire: Explorer thumbnails a HEIC, an AVIF, `pq_lossless.avif` (as SDR) and a cubemap DDS (every
    face) through the new `fire_shellext.dll`; the viewer draws an EXR and a PQ file through both
    tone curves; an iPhone HEIC in HDR mode (`H`) shows brighter highlights than its base, and `H`
    again returns to it; `seq_hevc.heif` plays three times and stops, `seq_infinite.avif` loops,
    and leaving `seq_over_cap.avif` mid-decode frees the pool at once.
  - 3D Review: a HEIC and an AVIF plane (`local/win/*_plane.fbx`) in the viewer, and in
    Explorer's thumbnail and preview pane through the new `review_shell.dll`.
  - bite: install `dist/bite-0.5.3.exe` and import a HEIC folder through the HEIC chip.
  - **A transparent PSD saved by Photoshop:** a layered document with no Background layer and
    some soft-edged transparency, saved with Maximize Compatibility. Compare Photoshop's view
    with fire-decode's merged image and ImageMagick's. Does the merged colour carry a white
    matte that the crate should divide out (plan §6, PSD)?
- **fire moves to the tag** (item 7), by merging its `fire-decode` branch, which already drops the
  three in-tree crates and carries the installer test (FD2, FD9). Then:
  - `fire-decode = { git = "https://github.com/psmyles/fire-decode", tag = "v0.1.0" }` in place of
    `branch = "main"`;
  - fire's CI follows: no `-p fire-decode` in the decoder-free job, no vendored-artifact checks or
    LLVM install in the native one (the crate brings its libraries, and bindgen is gone), and a
    toolchain of at least 1.99, the crate's `rust-version`. Its `stable` here is 1.98.1 too, so the
    Windows builds used `+1.99.1`, and fire's installer script, which runs plain `cargo`, needs
    the same;
  - fire's notices point at this repository's tree at the tag, with libheif / libde265 as
    LGPL-3.0-or-later, not `-only` (FD14).
- **3D Review and bite move to the tag** the same way, merging their `fire-decode` branches.

**Windows notes for whoever runs this again:**
- `RUSTFLAGS="-C target-feature=+crt-static"` for anything built outside an app's tree: this
  repository's tests, the benches, scratch tools. Without it heif-sys stops with its named error.
- Git Bash has no `strings`: `grep -a -c` does the plugin-path check.
- `pwsh -File` passes a comma list as one string: `local/ttfp-run.ps1` holds the file sets.
- 3D Review's `check.ps1` stops at the first failing test binary; `--no-fail-fast` shows the rest.

## References

- [`fire-decode-plan.md`](../fire-decode-plan.md).
- `docs/asset-check-plan.md` in SandboxVR/exp-content-rules: the Asset Check design.
- `docs/PLAN.md` in psmyles/fire, psmyles/3d-review, psmyles/asset-rules, psmyles/launcher and
  SandboxVR/exp-content-rules.
