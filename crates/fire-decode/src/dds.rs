//! DDS (DirectDraw Surface), decompressed on the CPU into the ordinary RGBA pipeline.
//!
//! DDS is a container, not a codec: a 124-byte header (plus a 20-byte DX10 extension on modern
//! files) followed by raw surface data in one of ~100 pixel formats. The two halves are split
//! here the same way. `ddsfile` parses the header, and this module maps it onto the codecs the
//! texture containers share ([`crate::texture`]), where `bcdec_rs` — a safe port of iOrange's
//! `bcdec` — decodes the block-compressed payloads. Both are pure Rust, so this backend needs no
//! vendored native tree and builds on every target the viewer does.
//!
//! What the formats map to:
//!
//! | Source | Output |
//! |---|---|
//! | BC1/BC2/BC3/BC7 (and their sRGB spellings) | `Rgba8Unorm` |
//! | BC4 | `Rgba8Unorm`, the one channel replicated to grey |
//! | BC5 | `Rgba8Unorm` as `(r, g, 0, 1)` — the normal-map layout, shown as authored |
//! | BC6H | `Rgba16Float` — the HDR path, so exposure and tonemap apply |
//! | 16-bit and half-float channels | `Rgba16Unorm` / `Rgba16Float` |
//! | 32-bit float, and the packed floats (R11G11B10, RGB9E5) | `Rgba32Float` |
//! | everything else uncompressed | via the header's channel bit masks |
//!
//! BC6H is the first producer of [`PixelFormat::Rgba16Float`], which the renderer has always
//! understood but nothing decoded to.
//!
//! Signed block formats (`BC4_SNorm`, `BC5_SNorm`) are re-centred for display: `bcdec` hands
//! back the raw -127..127 range reinterpreted as bytes, which would show a normal map as noise,
//! so they are mapped to 0..255 the way a sampler would.
//!
//! The file's own mip chain comes along in [`DecodedImage::source_mips`], and a file holding more
//! than one surface — a cubemap's six faces, a texture array's layers, a volume's depth slices —
//! is composited into a near-square sheet and described by a [`SheetLayout`], which is what lets
//! the viewer's flipbook step through them with the transport it already has.

use ddsfile::{Dds, DxgiFormat, FourCC, PixelFormatFlags};

use crate::texture::{self, Block, Format, Plain, Shape, decode_surface, mask_width, masked};
use crate::{Cx, DecodeError, DecodedImage, PixelFormat, SheetKind, SheetLayout, SourceInfo};

// --- format identification -----------------------------------------------------------------

/// Identify the payload from the header. `None` for a format we have no decoder for, which the
/// caller turns into a `Malformed` naming it.
fn identify(dds: &Dds) -> Option<Format> {
    let spf = &dds.header.spf;
    if spf.flags.contains(PixelFormatFlags::FOURCC) {
        let cc = spf.fourcc.as_ref()?.0;
        if cc == FourCC::DX10 {
            return from_dxgi(dds.header10.as_ref()?.dxgi_format);
        }
        return from_fourcc(cc);
    }
    from_masks(spf)
}

/// The DX10 header's `DxgiFormat`. Typeless variants decode as their unsigned-normalized
/// spelling: a viewer has to show *something*, and that is what every other tool picks.
fn from_dxgi(f: DxgiFormat) -> Option<Format> {
    use DxgiFormat as D;
    use PixelFormat::{Rgba8Unorm, Rgba16Float, Rgba16Unorm, Rgba32Float};
    Some(match f {
        D::BC1_Typeless | D::BC1_UNorm | D::BC1_UNorm_sRGB => {
            Format::block(Block::Bc1, Rgba8Unorm, 8, 4, "DDS BC1")
        }
        D::BC2_Typeless | D::BC2_UNorm | D::BC2_UNorm_sRGB => {
            Format::block(Block::Bc2, Rgba8Unorm, 8, 4, "DDS BC2")
        }
        D::BC3_Typeless | D::BC3_UNorm | D::BC3_UNorm_sRGB => {
            Format::block(Block::Bc3, Rgba8Unorm, 8, 4, "DDS BC3")
        }
        D::BC4_Typeless | D::BC4_UNorm => {
            Format::block(Block::Bc4(false), Rgba8Unorm, 8, 1, "DDS BC4")
        }
        D::BC4_SNorm => Format::block(Block::Bc4(true), Rgba8Unorm, 8, 1, "DDS BC4"),
        D::BC5_Typeless | D::BC5_UNorm => {
            Format::block(Block::Bc5(false), Rgba8Unorm, 8, 3, "DDS BC5")
        }
        D::BC5_SNorm => Format::block(Block::Bc5(true), Rgba8Unorm, 8, 3, "DDS BC5"),
        D::BC6H_Typeless | D::BC6H_UF16 => {
            Format::block(Block::Bc6h(false), Rgba16Float, 16, 3, "DDS BC6H")
        }
        D::BC6H_SF16 => Format::block(Block::Bc6h(true), Rgba16Float, 16, 3, "DDS BC6H"),
        D::BC7_Typeless | D::BC7_UNorm | D::BC7_UNorm_sRGB => {
            Format::block(Block::Bc7, Rgba8Unorm, 8, 4, "DDS BC7")
        }

        // 32-bit float.
        D::R32G32B32A32_Typeless | D::R32G32B32A32_Float => {
            Format::plain(Plain::F32(4), Rgba32Float, 32, 4, "DDS RGBA32F")
        }
        D::R32G32B32_Typeless | D::R32G32B32_Float => {
            Format::plain(Plain::F32(3), Rgba32Float, 32, 3, "DDS RGB32F")
        }
        D::R32G32_Typeless | D::R32G32_Float => {
            Format::plain(Plain::F32(2), Rgba32Float, 32, 3, "DDS RG32F")
        }
        D::R32_Typeless | D::R32_Float | D::D32_Float => {
            Format::plain(Plain::F32(1), Rgba32Float, 32, 1, "DDS R32F")
        }

        // Half float.
        D::R16G16B16A16_Typeless | D::R16G16B16A16_Float => {
            Format::plain(Plain::F16(4), Rgba16Float, 16, 4, "DDS RGBA16F")
        }
        D::R16G16_Float => Format::plain(Plain::F16(2), Rgba16Float, 16, 3, "DDS RG16F"),
        D::R16_Float => Format::plain(Plain::F16(1), Rgba16Float, 16, 1, "DDS R16F"),

        // 16-bit unsigned normalized.
        D::R16G16B16A16_UNorm => Format::plain(Plain::U16(4), Rgba16Unorm, 16, 4, "DDS RGBA16"),
        D::R16G16_UNorm => Format::plain(Plain::U16(2), Rgba16Unorm, 16, 3, "DDS RG16"),
        D::R16_UNorm | D::D16_UNorm => Format::plain(Plain::U16(1), Rgba16Unorm, 16, 1, "DDS R16"),

        // Packed floats.
        D::R11G11B10_Float => Format::plain(Plain::R11G11B10, Rgba32Float, 11, 3, "DDS R11G11B10F"),
        D::R9G9B9E5_SharedExp => Format::plain(Plain::R9G9B9E5, Rgba32Float, 9, 3, "DDS RGB9E5"),

        // Integer channels. The DX10 header names a channel order rather than bit masks, so the
        // masks are spelled out here and share the one mask-driven expander.
        D::R8G8B8A8_Typeless | D::R8G8B8A8_UNorm | D::R8G8B8A8_UNorm_sRGB => masked(
            4,
            [0xff, 0xff00, 0xff_0000, 0xff00_0000],
            false,
            "DDS RGBA8",
        ),
        D::B8G8R8A8_Typeless | D::B8G8R8A8_UNorm | D::B8G8R8A8_UNorm_sRGB => masked(
            4,
            [0xff_0000, 0xff00, 0xff, 0xff00_0000],
            false,
            "DDS BGRA8",
        ),
        D::B8G8R8X8_Typeless | D::B8G8R8X8_UNorm | D::B8G8R8X8_UNorm_sRGB => {
            masked(4, [0xff_0000, 0xff00, 0xff, 0], false, "DDS BGRX8")
        }
        D::R10G10B10A2_Typeless | D::R10G10B10A2_UNorm => masked(
            4,
            [0x3ff, 0xf_fc00, 0x3ff0_0000, 0xc000_0000],
            false,
            "DDS RGB10A2",
        ),
        D::R8G8_Typeless | D::R8G8_UNorm => masked(2, [0xff, 0xff00, 0, 0], false, "DDS RG8"),
        D::R8_Typeless | D::R8_UNorm => masked(1, [0xff, 0, 0, 0], true, "DDS R8"),
        D::A8_UNorm => masked(1, [0, 0, 0, 0xff], true, "DDS A8"),
        D::B5G6R5_UNorm => masked(2, [0xf800, 0x7e0, 0x1f, 0], false, "DDS B5G6R5"),
        D::B5G5R5A1_UNorm => masked(2, [0x7c00, 0x3e0, 0x1f, 0x8000], false, "DDS B5G5R5A1"),
        D::B4G4R4A4_UNorm => masked(2, [0xf00, 0xf0, 0xf, 0xf000], false, "DDS B4G4R4A4"),

        _ => return None,
    })
}

/// A legacy FourCC. `DXT2`/`DXT4` are `DXT3`/`DXT5` with premultiplied alpha — the same blocks,
/// straightened afterwards by [`decode`] — and the `ATI1`/`ATI2`/`BC4?`/`BC5?`/`3DC?` family is
/// how BC4 and BC5 were spelled before the DX10 header existed.
fn from_fourcc(cc: u32) -> Option<Format> {
    use PixelFormat::{Rgba8Unorm, Rgba16Float, Rgba16Unorm, Rgba32Float};
    const fn code(s: &[u8; 4]) -> u32 {
        (s[0] as u32) | ((s[1] as u32) << 8) | ((s[2] as u32) << 16) | ((s[3] as u32) << 24)
    }
    Some(match cc {
        FourCC::DXT1 => Format::block(Block::Bc1, Rgba8Unorm, 8, 4, "DDS BC1"),
        FourCC::DXT2 | FourCC::DXT3 => Format::block(Block::Bc2, Rgba8Unorm, 8, 4, "DDS BC2"),
        FourCC::DXT4 | FourCC::DXT5 => Format::block(Block::Bc3, Rgba8Unorm, 8, 4, "DDS BC3"),
        FourCC::ATI1 => Format::block(Block::Bc4(false), Rgba8Unorm, 8, 1, "DDS BC4"),
        FourCC::ATI2 => Format::block(Block::Bc5(false), Rgba8Unorm, 8, 3, "DDS BC5"),
        FourCC::A16B16G16R16 => Format::plain(Plain::U16(4), Rgba16Unorm, 16, 4, "DDS RGBA16"),
        FourCC::R16F => Format::plain(Plain::F16(1), Rgba16Float, 16, 1, "DDS R16F"),
        FourCC::G16R16F => Format::plain(Plain::F16(2), Rgba16Float, 16, 3, "DDS RG16F"),
        FourCC::A16B16G16R16F => Format::plain(Plain::F16(4), Rgba16Float, 16, 4, "DDS RGBA16F"),
        FourCC::R32F => Format::plain(Plain::F32(1), Rgba32Float, 32, 1, "DDS R32F"),
        FourCC::G32R32F => Format::plain(Plain::F32(2), Rgba32Float, 32, 3, "DDS RG32F"),
        FourCC::A32B32G32R32F => Format::plain(Plain::F32(4), Rgba32Float, 32, 4, "DDS RGBA32F"),
        c if c == code(b"BC4U") || c == code(b"3DC1") => {
            Format::block(Block::Bc4(false), Rgba8Unorm, 8, 1, "DDS BC4")
        }
        c if c == code(b"BC4S") => Format::block(Block::Bc4(true), Rgba8Unorm, 8, 1, "DDS BC4"),
        c if c == code(b"BC5U") || c == code(b"3DC2") => {
            Format::block(Block::Bc5(false), Rgba8Unorm, 8, 3, "DDS BC5")
        }
        c if c == code(b"BC5S") => Format::block(Block::Bc5(true), Rgba8Unorm, 8, 3, "DDS BC5"),
        _ => return None,
    })
}

/// A header that describes its channels with bit masks rather than a FourCC.
fn from_masks(spf: &ddsfile::PixelFormat) -> Option<Format> {
    let bits = spf.rgb_bit_count?;
    if !matches!(bits, 8 | 16 | 24 | 32) {
        return None;
    }
    let masks = [
        spf.r_bit_mask.unwrap_or(0),
        spf.g_bit_mask.unwrap_or(0),
        spf.b_bit_mask.unwrap_or(0),
        spf.a_bit_mask.unwrap_or(0),
    ];
    let widest = masks.iter().copied().map(mask_width).max()?;
    // A mask wider than the unit it sits in, or none at all, is not a layout we can read.
    if widest == 0
        || widest > 16
        || masks
            .iter()
            .any(|&m| m != 0 && m.leading_zeros() < 32 - bits)
    {
        return None;
    }
    let luminance = spf.flags.contains(PixelFormatFlags::LUMINANCE);
    Some(masked((bits / 8) as u8, masks, luminance, "DDS"))
}

/// What the header says the format is, for an error message. Falls back to the raw FourCC, then
/// to the bit masks, so an unsupported file says which format it was rather than just "no".
fn describe(dds: &Dds) -> String {
    if let Some(h10) = dds.header10.as_ref() {
        return format!("{:?}", h10.dxgi_format);
    }
    match dds.header.spf.fourcc.as_ref().map(|f| f.0) {
        Some(cc) if cc != FourCC::NONE => {
            let b = cc.to_le_bytes();
            if b.iter().all(u8::is_ascii_graphic) {
                format!("FourCC {}", String::from_utf8_lossy(&b))
            } else {
                format!("FourCC {cc:#x}")
            }
        }
        _ => format!(
            "{}-bit masks R={:#x} G={:#x} B={:#x} A={:#x}",
            dds.header.spf.rgb_bit_count.unwrap_or(0),
            dds.header.spf.r_bit_mask.unwrap_or(0),
            dds.header.spf.g_bit_mask.unwrap_or(0),
            dds.header.spf.b_bit_mask.unwrap_or(0),
            dds.header.spf.a_bit_mask.unwrap_or(0),
        ),
    }
}

// --- surface layout ------------------------------------------------------------------------

/// Where every surface sits in the file's data blob.
///
/// A DDS packs its surfaces tightly, array layer by array layer (a cubemap is six layers, in
/// `+X -X +Y -Y +Z -Z` order), and within a layer mip level by mip level, largest first. Each
/// level halves both axes, flooring but never going below 1 — the same rule the viewer's own mip
/// builder uses, which is what will let the file's chain be adopted verbatim.
struct Stored {
    levels: u32,
    layers: u32,
    /// Byte size of each mip level of one layer, level 0 first.
    level_sizes: Vec<usize>,
    /// Bytes from the start of one layer to the start of the next.
    layer_stride: usize,
}

impl Stored {
    fn new(dds: &Dds, fmt: &Format, data_len: usize) -> Result<Stored, DecodeError> {
        let (w, h) = (dds.header.width, dds.header.height);
        let depth = dds.get_depth().max(1);
        // A header may claim more levels than the dimensions can have, and a corrupt one may
        // claim billions. Either way the chain stops where the halving reaches 1x1.
        let max_levels = 32 - w.max(h).max(1).leading_zeros();
        let levels = dds.get_num_mipmap_levels().clamp(1, max_levels);

        let mut level_sizes = Vec::with_capacity(levels as usize);
        let mut layer_stride = 0usize;
        let (mut lw, mut lh, mut ld) = (w, h, depth);
        for _ in 0..levels {
            let n = fmt.level_bytes(lw, lh, ld).ok_or_else(|| {
                DecodeError::TooLarge(format!("DDS {w}x{h} mip chain overflows a usize"))
            })?;
            layer_stride = layer_stride.checked_add(n).ok_or_else(|| {
                DecodeError::TooLarge(format!("DDS {w}x{h} mip chain overflows a usize"))
            })?;
            level_sizes.push(n);
            lw = (lw / 2).max(1);
            lh = (lh / 2).max(1);
            ld = (ld / 2).max(1);
        }

        // A cubemap header always claims six faces even when only some were written, and a DX10
        // `array_size` is attacker-controlled — so the layer count is what the data can actually
        // back, never what the header asks for.
        let declared = dds.get_num_array_layers().max(1);
        let backed = data_len
            .checked_div(layer_stride)
            .unwrap_or(0)
            .min(u32::MAX as usize) as u32;
        let layers = declared.min(backed);
        if layers == 0 {
            return Err(DecodeError::Malformed(format!(
                "DDS holds {data_len} bytes of surface data, short of the {layer_stride} its \
                 {w}x{h} header describes"
            )));
        }
        Ok(Stored {
            levels,
            layers,
            level_sizes,
            layer_stride,
        })
    }

    /// Byte range of one surface within the data blob.
    fn range(&self, layer: u32, level: u32) -> Option<std::ops::Range<usize>> {
        if layer >= self.layers || level >= self.levels {
            return None;
        }
        let start = (layer as usize).checked_mul(self.layer_stride)?
            + self.level_sizes[..level as usize].iter().sum::<usize>();
        Some(start..start + self.level_sizes[level as usize])
    }
}

// --- decoding ------------------------------------------------------------------------------

/// Decode a DDS from memory, normalized to RGBA.
///
/// A single-surface file decodes to its one surface. A cubemap, texture array or volume is, with
/// [`Surfaces::Sheet`](crate::Surfaces::Sheet), composited into one sheet and described by a
/// [`SheetLayout`]; with [`Surfaces::First`](crate::Surfaces::First) it decodes to its first
/// surface alone. The file's own mips come back only when the setup keeps them (`stored_mips`),
/// and are not decoded otherwise ([`texture::assemble`]).
pub fn decode(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let dds = Dds::read(bytes).map_err(|e| DecodeError::Malformed(format!("DDS: {e}")))?;
    let fmt = identify(&dds).ok_or_else(|| {
        DecodeError::Malformed(format!("unsupported DDS format: {}", describe(&dds)))
    })?;

    let (width, height) = (dds.header.width, dds.header.height);
    if width == 0 || height == 0 {
        return Err(DecodeError::Malformed(format!(
            "DDS declares a {width}x{height} surface"
        )));
    }

    // The guard runs on the header's bare claim first — one surface, and the sheet of every
    // layer the header declares — before the surface walk below reads anything else from it.
    let declared = layout_for(&dds, dds.get_num_array_layers().max(1));
    texture::check_texture(cx, fmt.out, width, height, declared, "DDS")?;

    let surfaces = Stored::new(&dds, &fmt, dds.data.len())?;
    // The layout the data actually backs, which is what gets tiled. `Stored` has already
    // clamped the layer count to the bytes present, so a header claiming a hundred layers over a
    // six-layer file lays out six.
    let shape = Shape {
        width,
        height,
        levels: surfaces.levels,
        all: layout_for(&dds, surfaces.layers),
    };
    let volume = shape.all.is_some_and(|l| l.kind == SheetKind::VolumeSlices);
    // `DXT2`/`DXT4`, and any DX10 header that says so, store colour already multiplied by alpha.
    let img = texture::assemble(
        cx,
        fmt.out,
        &shape,
        "DDS",
        if premultiplied(&dds) {
            texture::Alpha::Premultiplied
        } else {
            texture::Alpha::Straight
        },
        |level, frame, fw, fh| {
            surface_bytes(&dds, &surfaces, &fmt, volume, level, frame, fw, fh)
                .map(|src| decode_surface(&fmt, src, fw, fh))
        },
    )?;

    Ok(DecodedImage {
        pixels: img.pixels,
        format: fmt.out,
        source: SourceInfo::new(crate::Format::Dds, fmt.label, fmt.channels, fmt.bit_depth),
        icc: None,
        alpha_opaque: false, // set by `decode` after the final buffer is built
        width: img.width,
        height: img.height,
        downscaled_from: None,
        source_mips: img.source_mips,
        layout: img.layout,
        animation: None,
    })
}

/// The size [`decode`] returns for a DDS with [`Surfaces::Sheet`](crate::Surfaces::Sheet), from
/// the file's first bytes (`head`, its header at least) and its length: one surface, or the sheet
/// the data backs (FD19).
pub(crate) fn probe_dims(head: &[u8], len: usize) -> Result<(u32, u32), DecodeError> {
    let dds = Dds::read(head).map_err(|e| DecodeError::Malformed(format!("DDS: {e}")))?;
    let fmt = identify(&dds).ok_or_else(|| {
        DecodeError::Malformed(format!("unsupported DDS format: {}", describe(&dds)))
    })?;
    let (width, height) = (dds.header.width, dds.header.height);
    if width == 0 || height == 0 {
        return Err(DecodeError::Malformed(format!(
            "DDS declares a {width}x{height} surface"
        )));
    }
    // The magic and the header, and the DX10 extension when there is one.
    let header_len = 4 + 124 + if dds.header10.is_some() { 20 } else { 0 };
    let stored = Stored::new(&dds, &fmt, len.saturating_sub(header_len))?;
    let (cols, rows) = layout_for(&dds, stored.layers).map_or((1, 1), |l| (l.cols, l.rows));
    let too_large = || texture::too_large("DDS");
    Ok((
        width.checked_mul(cols).ok_or_else(too_large)?,
        height.checked_mul(rows).ok_or_else(too_large)?,
    ))
}

/// How a file's `layers` array layers are laid out, or `None` for the ordinary single-surface
/// case. Depth is read first: a volume is one array layer holding many slices, where a cubemap or
/// an array is many layers holding one surface each.
///
/// `layers` is a parameter rather than read from `dds` because it is asked twice with different
/// answers: the header's *declared* count for the size guard, then the count the data actually
/// backs for the tiling.
fn layout_for(dds: &Dds, layers: u32) -> Option<SheetLayout> {
    let depth = dds.get_depth().max(1);
    let (frames, kind) = if depth > 1 {
        (depth, SheetKind::VolumeSlices)
    } else if layers > 1 {
        let cube = dds.header10.as_ref().map_or_else(
            || dds.header.caps2.contains(ddsfile::Caps2::CUBEMAP),
            |h| h.misc_flag.contains(ddsfile::MiscFlag::TEXTURECUBE),
        );
        let kind = if cube {
            SheetKind::CubeFaces
        } else {
            SheetKind::ArrayLayers
        };
        (layers, kind)
    } else {
        return None;
    };
    texture::sheet_layout(frames, kind)
}

/// The bytes of one surface: an array layer of its own for a cubemap or an array (and, for a
/// single surface, layer 0), or one depth slice carved out of the single layer's level for a
/// volume.
#[allow(clippy::too_many_arguments)]
fn surface_bytes<'a>(
    dds: &'a Dds,
    surfaces: &Stored,
    fmt: &Format,
    volume: bool,
    level: u32,
    frame: u32,
    fw: u32,
    fh: u32,
) -> Option<&'a [u8]> {
    if volume {
        // A volume's level holds every slice back to back, so one slice is that level's range
        // divided by the slice count.
        let range = surfaces.range(0, level)?;
        let slice = fmt.level_bytes(fw, fh, 1)?;
        let start = range
            .start
            .checked_add((frame as usize).checked_mul(slice)?)?;
        let end = start.checked_add(slice)?;
        if end > range.end {
            return None;
        }
        dds.data.get(start..end)
    } else {
        dds.data.get(surfaces.range(frame, level)?)
    }
}

/// Whether the file says its colour channels are already multiplied by alpha.
fn premultiplied(dds: &Dds) -> bool {
    if let Some(h10) = dds.header10.as_ref() {
        return h10.alpha_mode == ddsfile::AlphaMode::PreMultiplied;
    }
    matches!(
        dds.header.spf.fourcc.as_ref().map(|f| f.0),
        Some(FourCC::DXT2 | FourCC::DXT4)
    )
}
