# KTX / KTX2 test files

Real encoder output for the `ktx` feature's tests: [`tests/ktx_fixtures.rs`](../../crates/fire-decode/tests/ktx_fixtures.rs)
reads them. They are a selection from KTX-Software's own test resources (`tests/resources/`),
taken at commit `983695c` (17 November 2026). 35 files, about 3.8 MB.

```sh
fixtures/ktx/fetch.sh           # fetch any that are missing, then check every file's SHA-256
fixtures/ktx/fetch.sh --check   # check the committed files only
```

Upstream keeps them in Git LFS, so `fetch.sh` takes them from `media.githubusercontent.com`. Each
checksum is the LFS object id KTX-Software records for the file, so a check proves the file is
byte for byte the one upstream tests with. [`tests/ktx.rs`](../../crates/fire-decode/tests/ktx.rs)
covers everything that can be written by hand (headers, guards, layouts, supercompression); these
are for what can't: ETC1S, UASTC, ETC2 and ASTC as their encoders write them.

## Licences

KTX-Software's [`REUSE.toml`](https://github.com/KhronosGroup/KTX-Software/blob/983695c105618462b8e96458b378222e99faec98/REUSE.toml)
puts every `.ktx`, `.ktx2` and `.png` file under **Apache-2.0** (© 2015-2022 The Khronos Group
Inc.), with exceptions it lists by name. One exception is used here:

- `ktx2/Desk_uastc_hdr4x4_zstd_15.ktx2` is **BSD-3-Clause**, © 2004 Industrial Light & Magic, a
  division of Lucasfilm Entertainment Company Ltd. (made from OpenEXR's `Desk.exr`).

Left out on purpose: `hi_mark*.ktx` (a trademark of HI Corporation), anything made from `kodim17`
(Kodak's licence), the GoldenGate and Yokohama cubemaps (CC-BY-3.0, which would need attribution
wherever the repository goes), and `skybox_zstd_22.ktx2` (CC0, but 13 MB).

## The files

| Files | What they hold | Tested for |
| --- | --- | --- |
| `color_grid.png`, `ktx2/color_grid_{zstd_5,uastc_zstd_5,blze}.ktx2` | A 1024² labelled grid, and it as Zstandard RGBA8, UASTC under Zstandard, and ETC1S under BasisLZ | Each against the PNG: exact, then within the codec's error |
| `ktx2/r8g8b8a8_srgb_mip{,_etc2,_astc,_blze}.ktx2` | One 64² image with its 7-level chain: RGBA8, ETC2, ASTC 4x4, ETC1S | Every level of each encoding against the original |
| `ktx2/{bc3,etc2,astc_8x8}_unorm_array_7.ktx2`, `ktx/` the same | A 7-layer 256² array in three block formats | A 3x3 sheet each; the three alike; KTX and KTX2 alike |
| `ktx2/r8g8b8a8_srgb_array_7_mip.ktx2`, `ktx2/r8g8b8a8_srgb_3d_7.ktx2` | A 7-layer array with mips; a 7-slice volume | The sheet's chain; a volume keeps none |
| `ktx2/orient_{up,down}_metadata.ktx2`, `ktx/` the same | One picture stored top-down and bottom-up, with `KTXorientation` saying which | All four decode to the same pixels |
| `ktx2/alpha_complex_{premultiplied,straight}.ktx2` | One sRGB picture with soft alpha, premultiplied (in linear light) and not | Straightened, the first matches the second |
| `ktx2/Desk_uastc_hdr4x4_zstd_15.ktx2` | An HDR photograph as UASTC HDR 4x4 under Zstandard | Half-float, with values past 1.0 |
| `ktx/etc1.ktx`, `ktx/etc2_{rgb,rgba1,rgba8}.ktx` | ETC1 and the three ETC2 formats | Labels and channels |
| `ktx/conftestimage_{,SIGNED_}{R11,RG11}_EAC.ktx` | Khronos' EAC conformance images | Grey and red-green, signed re-centred |
| `ktx/r8g8b8_srgb{,_mip}.ktx`, `ktx/not4_r8g8b8_srgb.ktx`, `ktx/l8_unorm_metadata.ktx` | RGB8 (with mips), RGB8 whose rows aren't a multiple of 4 bytes, luminance | KTX's 4-byte row padding; `GL_LUMINANCE` |
