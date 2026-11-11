# fire-decode

The image decoder shared by [Fire](https://github.com/psmyles/fire),
[3D Review](https://github.com/psmyles/3d-review) and [bite](https://github.com/psmyles/bite): one
Rust crate that turns the bytes of an image file into pixels, fast, on Windows and macOS.

- **Speed first.** Decode time is the project's main measure, gated before every release:
  threaded passes, an integer path for PSD, and one copy from libheif into the caller's buffer.
- **One entry point.** [`decode`] picks the decoder from the file's magic bytes. Each app asks
  for its pixels through a named setup instead of a pile of options.
- **Hostile files are expected.** Size limits stop decode bombs before anything is allocated.
  Malformed files come back as errors, and a panic inside a decoder comes back as an error too.

## What it reads

| Format | Notes |
| --- | --- |
| PNG, JPEG, BMP, TIFF, TGA, GIF, ICO / CUR, WebP, QOI, PNM (PPM, PGM, PBM, PAM, PFM), farbfeld, JPEG XL | 8 or 16-bit, as stored. GIF keeps every frame. EXIF orientation applied where the setup asks |
| OpenEXR, Radiance HDR | Half or 32-bit float, linear |
| DDS | BC1-BC7 (BC6H included) and the uncompressed layouts; cubemaps, arrays, volumes and stored mips |
| PSD | The merged image, 8, 16 or 32-bit, through a hardened psd_sdk (feature `psd`); its layers one at a time through `PsdLayers` |
| HEIF, HEIC, AVIF | Through libheif, libde265 and dav1d (feature `heif`): grids, `irot` / `imir`, 10 and 12-bit, PQ / HLG, Apple and ISO gain maps, image sequences |
| Camera raw | The embedded JPEG preview (feature `raw`) |

[`sniff`] names a file's format from its first few KiB, and [`probe`] reads its size and HDR kind
from the headers without decoding. `supported_extensions()` lists every extension the build
reads.

## Using it

The crate isn't on crates.io. Take it from this repository at a release tag. Each tag is `v` plus
the version in [`product.json`](product.json); the latest is on the
[releases page](https://github.com/psmyles/fire-decode/releases).

```toml
[dependencies]
fire-decode = { git = "https://github.com/psmyles/fire-decode", tag = "vX.Y.Z", features = ["psd", "heif"] }
```

**Features.** None are on by default: each app names what it uses.

| Feature | What it adds |
| --- | --- |
| `psd` | PSD, through psd_sdk (C++, compiled by `cc`) |
| `heif` | HEIF, HEIC and AVIF, through the vendored libheif, libde265 and dav1d |
| `icc` | ICC and nclx colour conversion to sRGB, through Little-CMS |
| `raw` | Camera raw previews |
| `mips` | `fire_decode::mips`, the mip-chain builder Fire and 3D Review upload textures with |
| `allow-abort` | Accept `panic = "abort"`, under which the panic guard can't work |

A format whose feature is off decodes to `DecodeError::Unsupported`.

**Platforms and toolchain:**
- Rust 1.99 or later.
- `heif` is for **x86_64 Windows (MSVC)** and **arm64 macOS** only: the prebuilt libraries are
  vendored for exactly those two. On any other target, the build stops with a named error.
- **On Windows, `heif` needs the static CRT.** Set it in the app's `.cargo/config.toml`, as all
  three apps do:
  ```toml
  [target.x86_64-pc-windows-msvc]
  rustflags = ["-C", "target-feature=+crt-static"]
  ```
  A `RUSTFLAGS` environment variable replaces that list, so a build that sets one must repeat the
  flag. Without it, heif-sys stops with an error that says so, rather than failing at link time.
- `psd` needs a C++17 compiler. The pure-Rust core needs nothing else.
- The crate catches a decoder's panic and returns it as `DecodeError::Panicked`. That needs
  `panic = "unwind"`: a `panic = "abort"` build fails to compile unless it enables `allow-abort`.

## Example

```rust
use std::fs::File;
use std::path::Path;

use fire_decode::{DecodeOptions, decode_path, probe};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new("photo.heic");

    // Size, format and HDR kind from the headers alone, without decoding.
    let header = probe(File::open(path)?, Some("heic"))?;
    println!("{}x{} {}", header.width, header.height, header.format.name());

    // The pixels, through the setup Fire's viewer uses.
    let image = decode_path(path, &DecodeOptions::viewer())?;
    println!("{}x{} {:?}, {} bytes", image.width, image.height, image.format, image.pixels.len());

    println!("fire-decode {}", fire_decode::VERSION);
    Ok(())
}
```

With bytes already in memory, call `fire_decode::decode(&bytes, ext_hint, &options)`.
`decode_with_progress` adds a progress callback and a cancel flag.

**The setups.** Each is a `DecodeOptions` with its own size limits. Adjust a field after
choosing one if you need to.

| Setup | For | What it gives |
| --- | --- | --- |
| `DecodeOptions::viewer()` | Fire's viewer | Colour-managed and upright; every animation frame and DDS surface; the file's own mips; shrunk only past 16384 px |
| `DecodeOptions::inspect()` | fire-cli, Asset Check | The file as stored: full size, no colour conversion, no EXIF rotation |
| `DecodeOptions::texture()` | 3D Review | As stored and unrotated, 8-bit unless the source is float, never shrunk |
| `DecodeOptions::thumbnail(size)` | bite | 8-bit, area-averaged to fit `size`, as stored |

**What comes back.** A `DecodedImage` holds interleaved RGBA as `Rgba8Unorm`, `Rgba16Unorm`,
`Rgba16Float` or `Rgba32Float`. It also carries the ICC profile, `source` (what the file was:
format, bit depth, channels, HDR kind), animation frames and stored mips.

**Errors.** `Malformed` means the file couldn't be read. `TooLarge` means the setup's limits
refused it. Never retry a `TooLarge` with another decoder in the same process: the limit exists
to keep that allocation from happening. bite retries failures through ImageMagick, which runs
in a process of its own.

**Threads.** Decodes draw on one process-wide budget, the cores less two but never below
`min(cores, 8)`. libheif's threads come from the same budget. `max_threads` caps a single
decode; set it when several decodes run at once, or the first one takes most of the cores.

## Working on it

```
crates/fire-decode/   the crate: routing, setups, colour, HDR, the area filter, mips
crates/psd-sdk-sys/   the psd_sdk bridge (C++), with fire-decode's hardening patches
crates/heif-sys/      the libheif bridge (C++ wrapper, versioned ABI) and the vendored libraries
fixtures/heif/        44 generated HEIF / AVIF test files and what a decoder must return
bench/                decode-formats (per-format timing), bite-import, png-to-exr
docs/                 the work plan, baselines and decoder notes
```

**Tests.** CI runs on macOS and Windows: `cargo fmt`, clippy on four feature sets, and the tests
on five. The sets are none, `psd` (tests only), `psd,heif`, `psd,heif,mips` and
`psd,heif,icc,raw,mips`. Locally:

```sh
cargo test -p fire-decode --features psd,heif,icc,raw,mips
cargo test -p psd-sdk-sys
```

On Windows, put `RUSTFLAGS="-C target-feature=+crt-static"` in front of both: this repository
has no `.cargo/config.toml` of its own, so that CI's macOS job can check the error a build
without it gets.

`tests/probe.rs` also checks real images when `FIRE_DECODE_CORPUS` lists folders, separated as
`PATH` is (`:`, or `;` on Windows). psd-sdk-sys's real-file test reads `T_Sides_D.psd` from a 3D
Review checkout beside this one.

**Trying a change in an app.** Patch the app's dependency for one command, run in the app's
folder with this checkout beside it:

```sh
cargo build --config 'patch."https://github.com/psmyles/fire-decode".fire-decode.path="../fire-decode/crates/fire-decode"'
```

Nothing outside the app's repository is involved, so no other build picks the patch up. The
build writes the path into the app's `Cargo.lock`: `git checkout Cargo.lock` afterwards, since a
`--locked` build refuses a lock that names a path.

**Performance.** Before a release, no format may be more than 2% slower than at the previous
tag. `bench/decode-formats` times every format in-process. Each app has its own end-to-end
measure: Fire's time to first pixel, 3D Review's gate, bite's import bench.
[`docs/baselines.md`](docs/baselines.md) has the method and every measurement so far.

**The HEIF libraries** are prebuilt static libraries committed under `crates/heif-sys/vendor/`,
one folder per target, built with vcpkg from a pinned baseline. Re-vendoring is deliberate, not
routine: [`crates/heif-sys/vendor/VENDOR.txt`](crates/heif-sys/vendor/VENDOR.txt) has the recipe,
the versions and the checks to run afterwards.

## Decoder versions

This repository sets the version of every decoder the apps link, and nothing else does. Cargo
builds an app from the app's own `Cargo.lock` and ignores this one, so a version range here would
let each app link versions of its own. The workspace [`Cargo.toml`](Cargo.toml) pins each decoder
exactly instead (`exr = "=1.74.3"`). An app at a tag links exactly what CI tested at that tag, and
an app that names one of these crates itself is held to the same version.

Pinned: every crate that reads a format, decompresses a format's data or converts colour. That
includes crates reached only through another one, which the crate names just to fix their
versions: `image`'s `png` and `gif`, the `jxl-*` crates under `jxl-oxide`, and `lcms2-sys`, which
is Little-CMS itself. General-purpose crates the apps share, such as `bytemuck`, `flate2` and
`rayon`, keep ranges and follow each app's lockfile.

**To update the decoders**, run the update script from the repository's root, then commit:

```sh
scripts/update-decoders.sh              # every pin; or name some: scripts/update-decoders.sh exr png
```
```powershell
& "$env:ProgramFiles\Git\bin\bash.exe" scripts/update-decoders.sh   # PowerShell
```

`cargo update` can't move an exact pin. The script loosens the pins, runs `cargo update` on them,
pins them again at the new versions, and lists what moved and any crate an update newly brought
in. A new major version, or a new minor before 1.0, changes the API: edit that pin by hand. Then
run the tests and the performance check, update [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md)
if `libwebp-sys` or `lcms2-sys` moved, and release.

## Versions and releases

[`product.json`](product.json) is the only place the crate's version, name and description are
set:
- the build script reads the version into `fire_decode::VERSION`;
- the release tag is `v` plus that version;
- CI fails any tag that doesn't match it.

The Cargo manifests carry no version of their own, so cargo reports 0.0.0; ignore it.

**To release a new version:**

1. Bump `"version"` in `product.json` (`MAJOR.MINOR.PATCH`), commit it, and push `main`.
2. Wait for CI to pass on that commit: `gh run watch`, or the Actions tab.
3. Gather the release assets into a folder. Every release carries the upstream source tarballs
   of the vendored libheif, libde265 and dav1d, so the LGPL's corresponding source stays
   available from this repository.
   - If the HEIF libraries haven't changed, take them from the previous release:
     ```sh
     gh release download v<previous version> --pattern '*.tar.gz' --dir <assets folder>
     ```
   - After a re-vendor, take them from vcpkg's `downloads/` folder.
4. Run the release script from the repository's root:
   ```sh
   scripts/release.sh <assets folder>              # macOS, or Git Bash on Windows
   ```
   ```powershell
   & "$env:ProgramFiles\Git\bin\bash.exe" scripts/release.sh <assets folder>   # PowerShell
   ```
   Add `--dry-run` to see the checks, the files and the notes without tagging anything.

The script checks that you are on `main`, that the tree is clean and pushed, and that CI passed
on that commit. It also checks the tag is new, both targets' libraries are present, and there is
one source tarball for each vendored library at the version `VENDOR.txt` records. Then it creates
and pushes the tag, and publishes the GitHub release. The release carries the tarballs, the vcpkg
triplets and `SHA256SUMS.txt`. It needs `git` and the GitHub CLI (`gh`), signed in.

The apps then move to a release by changing the `tag` in their `Cargo.toml` and running
`cargo update -p fire-decode`, which brings the release's decoder versions with it.

## Licence

fire-decode's own code is under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.

The native libraries it builds or links keep their own licences:
- libwebp is BSD-3-Clause;
- Little-CMS (`icc`) is MIT;
- psd_sdk (`psd`) is BSD-2-Clause;
- dav1d (`heif`) is BSD-2-Clause;
- libheif and libde265 (`heif`) are LGPL-3.0-or-later.

An app that enables `heif` links libheif and libde265 statically, so the LGPL's relinking terms
apply to it. [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) has the versions, the licence
texts and how to relink.

## More

- [`docs/PLAN.md`](docs/PLAN.md): the work plan, and where the migration stands.
- [`fire-decode-plan.md`](fire-decode-plan.md): the design, decisions FD1-FD20, as the three apps
  moved onto the crate.
- [`docs/baselines.md`](docs/baselines.md): every performance measurement, macOS and Windows.
- [`docs/decoders.md`](docs/decoders.md): the Rust decoder crates and their updates.

[`decode`]: crates/fire-decode/src/lib.rs
[`sniff`]: crates/fire-decode/src/lib.rs
[`probe`]: crates/fire-decode/src/probe.rs
