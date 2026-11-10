# HEIF / HEIC / AVIF test files

The HEIF fixtures from migration step 0 of [`fire-decode-plan.md`](../../fire-decode-plan.md). Every
picture is generated, so every file here may be redistributed. 44 files, about 770 KB.

```sh
python3 -I fixtures/heif/make.py           # rebuild every file, check each, rewrite expected.json
python3 -I fixtures/heif/make.py --check   # check the committed files, rebuilding nothing
```

- **To rebuild:** Homebrew's `libheif` (`heif-enc`, `heif-info`, `heif-dec`, with x265 and aom),
  ImageMagick's `magick`, numpy and Pillow. The gain-map files also need `swiftc` and macOS 15 or
  later: [`gainmaps.swift`](gainmaps.swift) writes them with ImageIO, Apple's own encoder.
- **To check:** the same tools, without Swift. ImageIO's reference values are then kept as committed.
- **What a check proves:**
  - libheif decodes each valid file back to what its source says;
  - this script reads each file's boxes itself (sizes, `irot` / `imir`, grid, nclx, items, box
    order), so a libheif mistake can't hide behind libheif's own report;
  - every malformed file fails in libheif.

[`expected.json`](expected.json) is what a decoder must return, file by file:
- the shown size and bit depth;
- the nclx colour, transforms and items;
- patches with their code values and the light they stand for: cd/m², and FD17's `Linear` value
  (cd/m² ÷ 203, in Rec.709 primaries);
- frames, delays and repetitions;
- for gain maps, ImageIO's own HDR rendering;
- or the error expected.

Steps 2b and 6 test against it. It also records the libheif and encoder versions used.

## The files

| Group | Files | What they hold | For |
| --- | --- | --- | --- |
| Bit depth | `depth{8,10,12}.{heic,avif}` | A 1024 px ramp with corner markers. 10 and 12-bit keep more than 256 levels along a row | 2b |
| Grid | `grid_iphone.heic` | 512 px tiles, 4×3, cropped to 2000×1500 as iPhone grids are, plus a 320 px thumbnail | 2b, FD8 tile threads |
| Orientation | `portrait_irot.heic`, `mirror_imir.heic`, `irot_imir.heic` | Coded so that, transformed as the boxes say, red is top-left | 2b, FD5 |
| PQ / HLG | `pq_lossless.{heic,avif}`, `hlg_lossless.{heic,avif}`, `pq_ycbcr.avif` | BT.2020, 10-bit, two rows of patches of known light: neutrals from 0 to 10,000 cd/m² (or signal 0-1), then colours. Lossless ones keep exact code values; `pq_ycbcr` is lossy matrix-9 YCbCr, as such files usually come | 2b (SDR rendering), 6b |
| Wide gamut | `p3_nclx.{heic,avif}` | Display P3 by nclx only, no ICC: primaries and two in-gamut colours | FD5 |
| Alpha | `premultiplied_alpha.heic` | Straight (200, 100, 50) at four alphas, stored premultiplied, lossless | FD16 |
| Sequences | `seq_infinite.avif` | 8 frames at 100 ms, infinite repetitions, plus a still primary (the first frame) | 6d, FD18 |
| | `seq_only.avif` | The same track with no `meta` box: no primary image | 2b, FD16 |
| | `seq_hevc.heif` | HEVC, 6 frames at 40 ms, played 3 times | 6d |
| | `seq_over_cap.avif` | 17 frames of 4096²: over the 1 GiB animation cap as RGBA8, so 16 frames and `truncated` | 6d |
| Box layout | `meta_after_mdat.{heic,avif}` | `ftyp`, `mdat`, then `meta`, offsets moved to match | FD19 |
| Gain maps | `gainmap_iso.heic`, `gainmap_iso_portrait.heic` | ISO 21496-1 (`tmap`), from an HDR picture whose patch `b` is 4× SDR white | 6c |
| | `gainmap_iso_tmap_primary.heic` | `gainmap_iso.heic` with `pitm` on the `tmap` item. libheif 1.23.6 can't decode it as primary | 6c, FD17 fallback |
| | `gainmap_apple.heic`, `gainmap_apple_portrait.heic` | Apple's HDR gain map, half size, headroom 4 in its XMP: full gain on patch `a`, 128 on `b` | 6c |
| | `gainmap_apple_portrait_unrotated_map.heic` | The portrait with the gain map's `irot` dropped: it must render the same, since the gain map's own transforms are ignored (FD17) | 6c |
| | `gainmap_both.heic` | ISO and Apple maps that disagree: ISO brightens `b`, Apple's `a` | 6c, "ISO wins" |
| Malformed | `bad_truncated_mdat`, `bad_truncated_meta`, `bad_iloc_past_eof`, `bad_hvcC`, `bad_grid_tiles`, `bad_not_heif`, `bad_ftyp_only` (`.heic`), `bad_av1_data.avif` | Each fails in libheif. The crate must return an error, never abort | 2b |
| Oversized | `big_ispe_40000.{heic,avif}`, `big_ispe_200000.heic` | `ispe` claims more than the coded 256×192 | 2b size guard |
| | `bomb_grid_refs.heic` | 80×80 references to one tile (40960²). libheif's default limit of 1000 references refuses it | 2b |
| | `bomb_grid_bytes.heic` | 31×31 references to one 10-bit 768 px tile: 23808², inside every per-axis limit and libheif's defaults. Only a byte limit catches it (4.2 GiB as RGBA16) | 2b, FD5 limits |

The oversized files are never decoded by `make.py`: a decoder without limits would try to allocate
what they claim.

## What building them showed

These bear on the plan:

- **Gain-map orientation (FD17, which now follows this).**
  - In ImageIO's ISO portrait, the base carries `irot 270` and is stored 256×192. The gain map is
    stored the same way (128×96) with `irot 0`. The `tmap` item has the rotated size (192×256) and
    `irot 0`.
  - ImageIO composites in stored orientation, then turns the result by the **base's** `irot`.
    Applying the *displayed* (`tmap`) item's transforms would leave this file unrotated.
  - For Apple's map, ImageIO writes `irot` on the gain map too, and renders the same with or without
    it.
  - Real iPhone portraits (below) do the same: gain map stored like the base, `tmap` rotated in size
    with `irot 0`.
- **ISO wins.** In `gainmap_both.heic`, ImageIO applies the ISO map and ignores Apple's.
- **Apple's reference values.** ImageIO renders full gain at headroom 4 as 4.003, and gain 128 as
  1.663. That is `1 + 3g` with `g` ≈ 0.22, close to 128 decoded through the sRGB curve.
- **libheif hands back premultiplied alpha as stored.** Un-premultiplying stays the caller's job,
  as fire-decode does today.
- **libheif can't decode a `tmap` primary item** (1.23.6), as FD17 expected.
- **`imir`'s axis:** 0 exchanges top and bottom, 1 left and right. This is the second edition's
  wording, and what libheif and libavif do. The first edition's "vertical axis" for 0 reads the
  other way.
- **Zeroing `av1C` is no error:** AV1 decoders read their configuration from the bitstream.

## Real iPhone photos

Three iPhone HEICs in bite's local `test_images` (not committed: they are photos) were read for
their layout. They show the following.

- **Both gain maps in every photo.** Apple's `hdrgainmap` auxiliary image (a grid at half size),
  and an ISO `tmap` item in an `altr` group. The `tmap` item takes Apple's gain-map image as its
  input, so the two maps share pixels.
- **The portrait:**
  - `irot 270` on the base, on the gain map and on every other auxiliary image (sky matte, linear
    thumbnail, style delta map);
  - `irot 0` on `tmap`, which has the rotated size.
- **Grids:** 512 px tiles at 12 MP (8×6). At 48 MP, 640×896 tiles (9×5).
- **Other auxiliary images** a decoder must skip: `semanticskymatte`, `linearthumbnail` and
  `styledeltamap`, plus a styles `uri ` metadata item.

## Not here

These belong in the local corpus, outside the repository, because they're photos or not ours to
redistribute:
- real iPhone HDR photos, beyond the three above (Apple's MakerNote headroom among them);
- Android ISO-gain-map AVIFs;
- ICC-tagged HEICs;
- large real photos for timing.
