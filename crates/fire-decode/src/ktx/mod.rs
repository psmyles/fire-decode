//! KTX and KTX2, Khronos' texture containers, decompressed on the CPU into the ordinary RGBA
//! pipeline the same way DDS is ([`crate::texture`]).
//!
//! KTX (v1) is the OpenGL-era container: a 64-byte header naming the format by its GL enums,
//! then each mip level's surfaces. KTX2 is its Vulkan-era successor, named by `VkFormat`, with a
//! level index, a data format descriptor, and optional *supercompression* — Zstandard or zlib
//! over each level, or BasisLZ, the codebook coding of Basis Universal's ETC1S. glTF's
//! `KHR_texture_basisu` textures are KTX2.
//!
//! Both containers name their formats through one table, [`from_vk`]: KTX2 by its `VkFormat`,
//! KTX by the `VkFormat` its GL enums correspond to. What the formats map to:
//!
//! | Source | Output |
//! |---|---|
//! | BC1-BC7, as DDS ([`crate::dds`]) | as DDS; `BC1_RGB` is opaque |
//! | ETC1, ETC2 RGB / RGBA1 / RGBA, ASTC LDR, PVRTC1 | `Rgba8Unorm` |
//! | EAC R11 / RG11 (and signed) | `Rgba8Unorm`, grey / `(r, g, 0, 1)` as BC4 / BC5 |
//! | Basis Universal ETC1S, UASTC | `Rgba8Unorm`, transcoded by `basisu` |
//! | UASTC HDR, ASTC HDR 6x6 (Basis) | `Rgba16Float` |
//! | 8-bit, packed, 16-bit, half and float layouts | as DDS's uncompressed ones |
//!
//! Integer (`UINT` / `SINT`), signed-normalized colour, stencil and the other ASTC HDR and 3D
//! formats are refused by name.
//!
//! **Orientation.** A KTX file may say which way its rows run (`KTXorientation`): OpenGL's
//! convention put row 0 at the bottom, and tools that kept it say so. The rows are turned so the
//! first one is the top, as every other backend returns them, whatever the setup: like HEIF's
//! `irot`, it is how to read the image, not a camera's opinion of it. glTF requires the default
//! (right, down), so a glTF texture never moves. `KTXswizzle` is not applied: the data is shown
//! as stored, as DDS is.

pub(crate) mod basis;
pub(crate) mod v2;

use crate::PixelFormat::{Rgba8Unorm, Rgba16Float, Rgba16Unorm, Rgba32Float};
use crate::texture::{Block, Format, Mobile, Plain, masked};
use crate::{DecodedImage, SheetKind, SheetLayout, SourceInfo};

/// Which container a format is named for: its status-bar label says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Container {
    // KTX (v1): its reader arrives after KTX2's.
    #[allow(dead_code)]
    V1,
    V2,
}

/// A literal label for `c`: `label!(c, "BC7")` is `"KTX BC7"` or `"KTX2 BC7"`.
macro_rules! label {
    ($c:expr, $($s:tt)+) => {
        match $c {
            Container::V1 => concat!("KTX ", $($s)+),
            Container::V2 => concat!("KTX2 ", $($s)+),
        }
    };
}

/// `VkFormat` values, and a few formats KTX (v1) can name that Vulkan has no spelling for.
#[allow(dead_code)]
pub(crate) mod vk {
    // The pseudo-formats sit above every `VkFormat` (whose extension values stay below 2^31).
    /// `GL_ETC1_RGB8_OES`: ETC1, which ETC2 RGB reads as a subset but which says it is ETC1.
    pub(crate) const ETC1: u32 = 0x8000_0001;
    /// `GL_LUMINANCE` and `GL_LUMINANCE_ALPHA`, 8 and 16 bits.
    pub(crate) const L8: u32 = 0x8000_0002;
    pub(crate) const L8A8: u32 = 0x8000_0003;
    pub(crate) const L16: u32 = 0x8000_0004;
    pub(crate) const L16A16: u32 = 0x8000_0005;

    pub(crate) const R4G4_UNORM_PACK8: u32 = 1;
    pub(crate) const R4G4B4A4_UNORM_PACK16: u32 = 2;
    pub(crate) const B4G4R4A4_UNORM_PACK16: u32 = 3;
    pub(crate) const R5G6B5_UNORM_PACK16: u32 = 4;
    pub(crate) const B5G6R5_UNORM_PACK16: u32 = 5;
    pub(crate) const R5G5B5A1_UNORM_PACK16: u32 = 6;
    pub(crate) const B5G5R5A1_UNORM_PACK16: u32 = 7;
    pub(crate) const A1R5G5B5_UNORM_PACK16: u32 = 8;
    pub(crate) const R8_UNORM: u32 = 9;
    pub(crate) const R8_SRGB: u32 = 15;
    pub(crate) const R8G8_UNORM: u32 = 16;
    pub(crate) const R8G8_SRGB: u32 = 22;
    pub(crate) const R8G8B8_UNORM: u32 = 23;
    pub(crate) const R8G8B8_SRGB: u32 = 29;
    pub(crate) const B8G8R8_UNORM: u32 = 30;
    pub(crate) const B8G8R8_SRGB: u32 = 36;
    pub(crate) const R8G8B8A8_UNORM: u32 = 37;
    pub(crate) const R8G8B8A8_SRGB: u32 = 43;
    pub(crate) const B8G8R8A8_UNORM: u32 = 44;
    pub(crate) const B8G8R8A8_SRGB: u32 = 50;
    pub(crate) const A8B8G8R8_UNORM_PACK32: u32 = 51;
    pub(crate) const A8B8G8R8_SRGB_PACK32: u32 = 57;
    pub(crate) const A2R10G10B10_UNORM_PACK32: u32 = 58;
    pub(crate) const A2B10G10R10_UNORM_PACK32: u32 = 64;
    pub(crate) const R16_UNORM: u32 = 70;
    pub(crate) const R16_SFLOAT: u32 = 76;
    pub(crate) const R16G16_UNORM: u32 = 77;
    pub(crate) const R16G16_SFLOAT: u32 = 83;
    pub(crate) const R16G16B16_UNORM: u32 = 84;
    pub(crate) const R16G16B16_SFLOAT: u32 = 90;
    pub(crate) const R16G16B16A16_UNORM: u32 = 91;
    pub(crate) const R16G16B16A16_SFLOAT: u32 = 97;
    pub(crate) const R32_SFLOAT: u32 = 100;
    pub(crate) const R32G32_SFLOAT: u32 = 103;
    pub(crate) const R32G32B32_SFLOAT: u32 = 106;
    pub(crate) const R32G32B32A32_SFLOAT: u32 = 109;
    pub(crate) const B10G11R11_UFLOAT_PACK32: u32 = 122;
    pub(crate) const E5B9G9R9_UFLOAT_PACK32: u32 = 123;
    pub(crate) const D16_UNORM: u32 = 124;
    pub(crate) const D32_SFLOAT: u32 = 126;
    pub(crate) const BC1_RGB_UNORM_BLOCK: u32 = 131;
    pub(crate) const BC1_RGB_SRGB_BLOCK: u32 = 132;
    pub(crate) const BC1_RGBA_UNORM_BLOCK: u32 = 133;
    pub(crate) const BC1_RGBA_SRGB_BLOCK: u32 = 134;
    pub(crate) const BC2_UNORM_BLOCK: u32 = 135;
    pub(crate) const BC2_SRGB_BLOCK: u32 = 136;
    pub(crate) const BC3_UNORM_BLOCK: u32 = 137;
    pub(crate) const BC3_SRGB_BLOCK: u32 = 138;
    pub(crate) const BC4_UNORM_BLOCK: u32 = 139;
    pub(crate) const BC4_SNORM_BLOCK: u32 = 140;
    pub(crate) const BC5_UNORM_BLOCK: u32 = 141;
    pub(crate) const BC5_SNORM_BLOCK: u32 = 142;
    pub(crate) const BC6H_UFLOAT_BLOCK: u32 = 143;
    pub(crate) const BC6H_SFLOAT_BLOCK: u32 = 144;
    pub(crate) const BC7_UNORM_BLOCK: u32 = 145;
    pub(crate) const BC7_SRGB_BLOCK: u32 = 146;
    pub(crate) const ETC2_R8G8B8_UNORM_BLOCK: u32 = 147;
    pub(crate) const ETC2_R8G8B8_SRGB_BLOCK: u32 = 148;
    pub(crate) const ETC2_R8G8B8A1_UNORM_BLOCK: u32 = 149;
    pub(crate) const ETC2_R8G8B8A1_SRGB_BLOCK: u32 = 150;
    pub(crate) const ETC2_R8G8B8A8_UNORM_BLOCK: u32 = 151;
    pub(crate) const ETC2_R8G8B8A8_SRGB_BLOCK: u32 = 152;
    pub(crate) const EAC_R11_UNORM_BLOCK: u32 = 153;
    pub(crate) const EAC_R11_SNORM_BLOCK: u32 = 154;
    pub(crate) const EAC_R11G11_UNORM_BLOCK: u32 = 155;
    pub(crate) const EAC_R11G11_SNORM_BLOCK: u32 = 156;
    /// The ASTC LDR formats run 157 (4x4 UNORM) to 184 (12x12 sRGB), UNORM and sRGB alternating
    /// through the 14 footprints in order of size: 4x4, 5x4, 5x5, 6x5, 6x6, 8x5, 8x6, 8x8, 10x5,
    /// 10x6, 10x8, 10x10, 12x10, 12x12.
    pub(crate) const ASTC_4X4_UNORM_BLOCK: u32 = 157;
    pub(crate) const ASTC_12X12_SRGB_BLOCK: u32 = 184;
    pub(crate) const ASTC_4X4_SFLOAT_BLOCK: u32 = 1_000_066_000;
    pub(crate) const ASTC_6X6_SFLOAT_BLOCK: u32 = 1_000_066_004;
    pub(crate) const ASTC_12X12_SFLOAT_BLOCK: u32 = 1_000_066_013;
    pub(crate) const PVRTC1_2BPP_UNORM_BLOCK: u32 = 1_000_054_000;
    pub(crate) const PVRTC1_4BPP_UNORM_BLOCK: u32 = 1_000_054_001;
    pub(crate) const PVRTC1_2BPP_SRGB_BLOCK: u32 = 1_000_054_004;
    pub(crate) const PVRTC1_4BPP_SRGB_BLOCK: u32 = 1_000_054_005;
    pub(crate) const A4R4G4B4_UNORM_PACK16: u32 = 1_000_340_000;
    pub(crate) const A4B4G4R4_UNORM_PACK16: u32 = 1_000_340_001;
    pub(crate) const A1B5G5R5_UNORM_PACK16: u32 = 1_000_470_000;
    pub(crate) const A8_UNORM: u32 = 1_000_470_001;
}

/// The format `vk` names, for container `c`; `None` for one this crate has no decoder for. The
/// sRGB and UNORM spellings decode alike, as DDS's do: the bytes are the same, and so is what
/// the viewer does with them.
pub(crate) fn from_vk(vk: u32, c: Container) -> Option<Format> {
    use self::vk::*;
    // The packed layouts are read most-significant component first: `R5G6B5` has red in the top
    // five bits. The unpacked ones are bytes in component order: `B8G8R8A8` is blue first.
    Some(match vk {
        BC1_RGB_UNORM_BLOCK | BC1_RGB_SRGB_BLOCK => {
            Format::block(Block::Bc1Rgb, Rgba8Unorm, 8, 3, label!(c, "BC1"))
        }
        BC1_RGBA_UNORM_BLOCK | BC1_RGBA_SRGB_BLOCK => {
            Format::block(Block::Bc1, Rgba8Unorm, 8, 4, label!(c, "BC1"))
        }
        BC2_UNORM_BLOCK | BC2_SRGB_BLOCK => {
            Format::block(Block::Bc2, Rgba8Unorm, 8, 4, label!(c, "BC2"))
        }
        BC3_UNORM_BLOCK | BC3_SRGB_BLOCK => {
            Format::block(Block::Bc3, Rgba8Unorm, 8, 4, label!(c, "BC3"))
        }
        BC4_UNORM_BLOCK => Format::block(Block::Bc4(false), Rgba8Unorm, 8, 1, label!(c, "BC4")),
        BC4_SNORM_BLOCK => Format::block(Block::Bc4(true), Rgba8Unorm, 8, 1, label!(c, "BC4")),
        BC5_UNORM_BLOCK => Format::block(Block::Bc5(false), Rgba8Unorm, 8, 3, label!(c, "BC5")),
        BC5_SNORM_BLOCK => Format::block(Block::Bc5(true), Rgba8Unorm, 8, 3, label!(c, "BC5")),
        BC6H_UFLOAT_BLOCK => {
            Format::block(Block::Bc6h(false), Rgba16Float, 16, 3, label!(c, "BC6H"))
        }
        BC6H_SFLOAT_BLOCK => {
            Format::block(Block::Bc6h(true), Rgba16Float, 16, 3, label!(c, "BC6H"))
        }
        BC7_UNORM_BLOCK | BC7_SRGB_BLOCK => {
            Format::block(Block::Bc7, Rgba8Unorm, 8, 4, label!(c, "BC7"))
        }

        ETC1 => Format::mobile(Mobile::Etc1, 8, 3, label!(c, "ETC1")),
        ETC2_R8G8B8_UNORM_BLOCK | ETC2_R8G8B8_SRGB_BLOCK => {
            Format::mobile(Mobile::Etc2Rgb, 8, 3, label!(c, "ETC2 RGB"))
        }
        ETC2_R8G8B8A1_UNORM_BLOCK | ETC2_R8G8B8A1_SRGB_BLOCK => {
            Format::mobile(Mobile::Etc2Rgba1, 8, 4, label!(c, "ETC2 RGBA1"))
        }
        ETC2_R8G8B8A8_UNORM_BLOCK | ETC2_R8G8B8A8_SRGB_BLOCK => {
            Format::mobile(Mobile::Etc2Rgba8, 8, 4, label!(c, "ETC2 RGBA"))
        }
        EAC_R11_UNORM_BLOCK => Format::mobile(Mobile::EacR(false), 11, 1, label!(c, "EAC R11")),
        EAC_R11_SNORM_BLOCK => Format::mobile(Mobile::EacR(true), 11, 1, label!(c, "EAC R11")),
        EAC_R11G11_UNORM_BLOCK => {
            Format::mobile(Mobile::EacRg(false), 11, 3, label!(c, "EAC RG11"))
        }
        EAC_R11G11_SNORM_BLOCK => Format::mobile(Mobile::EacRg(true), 11, 3, label!(c, "EAC RG11")),
        ASTC_4X4_UNORM_BLOCK..=ASTC_12X12_SRGB_BLOCK => astc(vk, c),
        PVRTC1_2BPP_UNORM_BLOCK | PVRTC1_2BPP_SRGB_BLOCK => Format::mobile(
            Mobile::Pvrtc1 { two_bpp: true },
            8,
            4,
            label!(c, "PVRTC1 2bpp"),
        ),
        PVRTC1_4BPP_UNORM_BLOCK | PVRTC1_4BPP_SRGB_BLOCK => Format::mobile(
            Mobile::Pvrtc1 { two_bpp: false },
            8,
            4,
            label!(c, "PVRTC1 4bpp"),
        ),

        // 8 bits a channel.
        R8_UNORM | R8_SRGB => masked(1, [0xff, 0, 0, 0], true, label!(c, "R8")),
        R8G8_UNORM | R8G8_SRGB => masked(2, [0xff, 0xff00, 0, 0], false, label!(c, "RG8")),
        R8G8B8_UNORM | R8G8B8_SRGB => {
            masked(3, [0xff, 0xff00, 0xff_0000, 0], false, label!(c, "RGB8"))
        }
        B8G8R8_UNORM | B8G8R8_SRGB => {
            masked(3, [0xff_0000, 0xff00, 0xff, 0], false, label!(c, "BGR8"))
        }
        R8G8B8A8_UNORM | R8G8B8A8_SRGB | A8B8G8R8_UNORM_PACK32 | A8B8G8R8_SRGB_PACK32 => masked(
            4,
            [0xff, 0xff00, 0xff_0000, 0xff00_0000],
            false,
            label!(c, "RGBA8"),
        ),
        B8G8R8A8_UNORM | B8G8R8A8_SRGB => masked(
            4,
            [0xff_0000, 0xff00, 0xff, 0xff00_0000],
            false,
            label!(c, "BGRA8"),
        ),
        A8_UNORM => masked(1, [0, 0, 0, 0xff], true, label!(c, "A8")),
        L8 => masked(1, [0xff, 0, 0, 0], true, label!(c, "L8")),
        L8A8 => masked(2, [0xff, 0, 0, 0xff00], true, label!(c, "LA8")),

        // Packed.
        R4G4_UNORM_PACK8 => masked(1, [0xf0, 0x0f, 0, 0], false, label!(c, "RG4")),
        R4G4B4A4_UNORM_PACK16 => masked(
            2,
            [0xf000, 0x0f00, 0x00f0, 0x000f],
            false,
            label!(c, "RGBA4"),
        ),
        B4G4R4A4_UNORM_PACK16 => masked(
            2,
            [0x00f0, 0x0f00, 0xf000, 0x000f],
            false,
            label!(c, "BGRA4"),
        ),
        A4R4G4B4_UNORM_PACK16 => masked(
            2,
            [0x0f00, 0x00f0, 0x000f, 0xf000],
            false,
            label!(c, "ARGB4"),
        ),
        A4B4G4R4_UNORM_PACK16 => masked(
            2,
            [0x000f, 0x00f0, 0x0f00, 0xf000],
            false,
            label!(c, "ABGR4"),
        ),
        R5G6B5_UNORM_PACK16 => masked(2, [0xf800, 0x07e0, 0x001f, 0], false, label!(c, "R5G6B5")),
        B5G6R5_UNORM_PACK16 => masked(2, [0x001f, 0x07e0, 0xf800, 0], false, label!(c, "B5G6R5")),
        R5G5B5A1_UNORM_PACK16 => masked(
            2,
            [0xf800, 0x07c0, 0x003e, 0x0001],
            false,
            label!(c, "RGB5A1"),
        ),
        B5G5R5A1_UNORM_PACK16 => masked(
            2,
            [0x003e, 0x07c0, 0xf800, 0x0001],
            false,
            label!(c, "BGR5A1"),
        ),
        A1R5G5B5_UNORM_PACK16 => masked(
            2,
            [0x7c00, 0x03e0, 0x001f, 0x8000],
            false,
            label!(c, "A1RGB5"),
        ),
        A1B5G5R5_UNORM_PACK16 => masked(
            2,
            [0x001f, 0x03e0, 0x7c00, 0x8000],
            false,
            label!(c, "A1BGR5"),
        ),
        A2R10G10B10_UNORM_PACK32 => masked(
            4,
            [0x3ff0_0000, 0x000f_fc00, 0x0000_03ff, 0xc000_0000],
            false,
            label!(c, "A2RGB10"),
        ),
        A2B10G10R10_UNORM_PACK32 => masked(
            4,
            [0x0000_03ff, 0x000f_fc00, 0x3ff0_0000, 0xc000_0000],
            false,
            label!(c, "A2BGR10"),
        ),
        B10G11R11_UFLOAT_PACK32 => Format::plain(
            Plain::R11G11B10,
            Rgba32Float,
            11,
            3,
            label!(c, "B10G11R11F"),
        ),
        E5B9G9R9_UFLOAT_PACK32 => {
            Format::plain(Plain::R9G9B9E5, Rgba32Float, 9, 3, label!(c, "RGB9E5"))
        }

        // 16 bits a channel, half and float. Two-channel sources report 3 channels, as DDS's do:
        // they are shown as RGB with a zero blue lane.
        R16_UNORM | D16_UNORM => Format::plain(Plain::U16(1), Rgba16Unorm, 16, 1, label!(c, "R16")),
        R16G16_UNORM => Format::plain(Plain::U16(2), Rgba16Unorm, 16, 3, label!(c, "RG16")),
        R16G16B16_UNORM => Format::plain(Plain::U16(3), Rgba16Unorm, 16, 3, label!(c, "RGB16")),
        R16G16B16A16_UNORM => Format::plain(Plain::U16(4), Rgba16Unorm, 16, 4, label!(c, "RGBA16")),
        L16 => masked(2, [0xffff, 0, 0, 0], true, label!(c, "L16")),
        L16A16 => masked(4, [0xffff, 0, 0, 0xffff_0000], true, label!(c, "LA16")),
        R16_SFLOAT => Format::plain(Plain::F16(1), Rgba16Float, 16, 1, label!(c, "R16F")),
        R16G16_SFLOAT => Format::plain(Plain::F16(2), Rgba16Float, 16, 3, label!(c, "RG16F")),
        R16G16B16_SFLOAT => Format::plain(Plain::F16(3), Rgba16Float, 16, 3, label!(c, "RGB16F")),
        R16G16B16A16_SFLOAT => {
            Format::plain(Plain::F16(4), Rgba16Float, 16, 4, label!(c, "RGBA16F"))
        }
        R32_SFLOAT | D32_SFLOAT => {
            Format::plain(Plain::F32(1), Rgba32Float, 32, 1, label!(c, "R32F"))
        }
        R32G32_SFLOAT => Format::plain(Plain::F32(2), Rgba32Float, 32, 3, label!(c, "RG32F")),
        R32G32B32_SFLOAT => Format::plain(Plain::F32(3), Rgba32Float, 32, 3, label!(c, "RGB32F")),
        R32G32B32A32_SFLOAT => {
            Format::plain(Plain::F32(4), Rgba32Float, 32, 4, label!(c, "RGBA32F"))
        }
        _ => return None,
    })
}

/// An ASTC LDR `VkFormat` (157..=184): the footprint from its place in the run.
fn astc(vk: u32, c: Container) -> Format {
    macro_rules! astc {
        ($w:literal, $h:literal) => {
            Format::mobile(Mobile::Astc($w, $h), 8, 4, label!(c, "ASTC ", $w, "x", $h))
        };
    }
    match (vk - vk::ASTC_4X4_UNORM_BLOCK) / 2 {
        0 => astc!(4, 4),
        1 => astc!(5, 4),
        2 => astc!(5, 5),
        3 => astc!(6, 5),
        4 => astc!(6, 6),
        5 => astc!(8, 5),
        6 => astc!(8, 6),
        7 => astc!(8, 8),
        8 => astc!(10, 5),
        9 => astc!(10, 6),
        10 => astc!(10, 8),
        11 => astc!(10, 10),
        12 => astc!(12, 10),
        _ => astc!(12, 12),
    }
}

/// The name a `VkFormat` goes by, for an error: `R8G8B8A8_UINT`, or its number.
pub(crate) fn describe_vk(vk: u32) -> String {
    match ktx2::Format::new(vk) {
        Some(f) => format!("{f:?}"),
        None => "VK_FORMAT_UNDEFINED".into(),
    }
}

/// How a file's surfaces tile: `frames` of them, a volume's slices, a cubemap's faces (`faces`
/// 6, layer by layer for a cube array) or an array's layers. `None` for a single surface.
pub(crate) fn layout(depth: u32, layers: u32, faces: u32) -> Option<SheetLayout> {
    let (frames, kind) = if depth > 1 {
        (depth, SheetKind::VolumeSlices)
    } else if faces == 6 {
        (layers.checked_mul(6)?, SheetKind::CubeFaces)
    } else {
        (layers, SheetKind::ArrayLayers)
    };
    crate::texture::sheet_layout(frames, kind)
}

/// Which way a file's rows and columns run, from its `KTXorientation`: `flip_y` when row 0 is the
/// bottom (`T=u`, `u`), `flip_x` when column 0 is the right (`S=l`, `l`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Orientation {
    pub(crate) flip_x: bool,
    pub(crate) flip_y: bool,
}

impl Orientation {
    /// KTX2's value: a character per axis, `r`/`l` then `d`/`u` (then `o`/`i` for depth, which a
    /// sheet of slices has no use for). Anything unexpected is the default.
    pub(crate) fn from_ktx2(value: &[u8]) -> Self {
        Orientation {
            flip_x: value.first() == Some(&b'l'),
            flip_y: value.get(1) == Some(&b'u'),
        }
    }

    /// Turn one decoded `w`x`h` cell of `bpp`-byte pixels upright, in place.
    pub(crate) fn apply(self, cell: &mut [u8], w: u32, h: u32, bpp: usize) {
        let row = w as usize * bpp;
        if row == 0 || cell.len() < row * h as usize {
            return;
        }
        if self.flip_y {
            let h = h as usize;
            for y in 0..h / 2 {
                let (top, bottom) = cell.split_at_mut((h - 1 - y) * row);
                top[y * row..y * row + row].swap_with_slice(&mut bottom[..row]);
            }
        }
        if self.flip_x {
            for r in cell.chunks_exact_mut(row) {
                let w = w as usize;
                for x in 0..w / 2 {
                    let (a, b) = r.split_at_mut((w - 1 - x) * bpp);
                    a[x * bpp..x * bpp + bpp].swap_with_slice(&mut b[..bpp]);
                }
            }
        }
    }
}

/// The finished image, from what [`assemble`](crate::texture::assemble) built.
pub(crate) fn image(
    img: crate::texture::Assembled,
    format: crate::Format,
    out: crate::PixelFormat,
    label: &'static str,
    channels: u8,
    bit_depth: u8,
) -> DecodedImage {
    DecodedImage {
        pixels: img.pixels,
        format: out,
        source: SourceInfo::new(format, label, channels, bit_depth),
        icc: None,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        width: img.width,
        height: img.height,
        downscaled_from: None,
        source_mips: img.source_mips,
        layout: img.layout,
        animation: None,
    }
}
