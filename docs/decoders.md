# Decoder versions

Every library that decodes pixels in fire-decode, the version in use, and upstream's latest when
last checked. Checked 10 November 2026, before the first tag.

## Rust crates

From `Cargo.lock`; "latest" is crates.io's newest stable release.

| Crate | Decodes | In use | Latest | |
| --- | --- | --- | --- | --- |
| `zune-jpeg`, `zune-core`, `zune-image` | JPEG (hot path) | 0.5.15, 0.5.3, 0.5.0 | same | Updated: zune-core 0.5.1 → 0.5.3 |
| `zune-bmp`, `zune-ppm`, `zune-farbfeld` | BMP, PPM / PGM / PFM, farbfeld | 0.5.2, 0.5.1, 0.5.2 | same | |
| `jxl-oxide` | JPEG XL | 0.12.6 | same | |
| `qoi` | QOI | 0.4.1 | same | |
| `image` | PNG, GIF, Radiance HDR, TGA, ICO, fallback | 0.25.10 | same | |
| `png` | PNG (through `image`) | 0.18.1 | same | |
| `gif` | GIF (through `image`) | 0.14.2 | same | |
| `tiff` | TIFF | 0.11.3 | same | |
| `exr` | OpenEXR | 1.74.3 | same | Updated from 1.74.0 |
| `image-webp` | lossless WebP | 0.2.4 | same | |
| `libwebp-sys` | lossy WebP (libwebp, below) | 0.14.4 | same | |
| `ddsfile`, `bcdec_rs` | DDS header, BC1-BC7 blocks | 0.6.0, 0.2.1 | same | Updated: bcdec_rs 0.2.0 → 0.2.1 |
| `lcms2`, `lcms2-sys` | ICC conversion (Little-CMS, below) | 6.2.0, 4.0.7 | same | Updated from 6.1.1, 4.0.6 |
| `miniz_oxide` | zlib for PSD layer channels | 0.8.9 | 0.9.1 | Stays: `exr` and `png` use 0.8 too |
| `flate2` | deflate in TIFF and PNG (through `tiff`, `png`) | 1.1.11 | same | Updated from 1.1.9 |

## Native libraries

| Library | Decodes | In use | Latest upstream | |
| --- | --- | --- | --- | --- |
| libheif | HEIF / HEIC / AVIF container | 1.23.7 | 1.23.7 (26 October 2026) | macOS re-vendored in step 1; Windows still to do |
| libde265 | HEVC | 1.1.3 | 1.1.3 (14 September 2026) | as libheif |
| dav1d | AV1 | 1.5.4 | 1.5.4 (14 July 2026) | as libheif |
| libwebp | lossy WebP | 1.6.0 | 1.6.0 | bundled by `libwebp-sys` |
| Little-CMS | ICC | 2.19 | 2.20 (31 October 2026) | bundled by `lcms2-sys`, whose newest (4.0.7) is still 2.19 |
| psd_sdk | PSD merged image | `f514495` + patches | `bfa1173` (11 August 2026) | Stays; see below |

**Why two aren't the latest:**
- **Little-CMS 2.20** reaches fire-decode only through a `lcms2-sys` release. Take it when one
  ships, rather than vendoring Little-CMS here.
- **psd_sdk's three later commits** add PSB and smart objects to its document and layer parsers.
  They don't touch the two files the merged-image decode goes through (`PsdParseImageDataSection.cpp`,
  `PsdDecompressRle.cpp`), so they would bring a rewritten layer parser fire-decode never calls, and
  no PSB merged image. See `crates/psd-sdk-sys/vendor/VENDOR.txt`.

## How to check again

```sh
# Rust crates: what `cargo update` would move, and what's past a semver boundary.
cargo update --dry-run --verbose

# Native libraries.
gh api repos/strukturag/libheif/releases/latest -q .tag_name
gh api repos/strukturag/libde265/releases/latest -q .tag_name
curl -s 'https://code.videolan.org/api/v4/projects/videolan%2Fdav1d/repository/tags?per_page=1'
gh api repos/mm2/Little-CMS/releases/latest -q .tag_name
gh api 'repos/MolecularMatters/psd_sdk/commits?per_page=1' -q '.[0].sha'
```

An update lands like any other change: the tests, then the TTFP comparison against the baseline
binary in `docs/baselines.md`, before the next tag (FD13).
