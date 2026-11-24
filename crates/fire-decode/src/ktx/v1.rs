//! KTX (v1): a 64-byte header naming the format by its OpenGL enums, key/value data, then each
//! mip level as a 4-byte `imageSize` followed by its surfaces — every array element, then every
//! face, then every depth slice. No supercompression and no data format descriptor; read here
//! by hand, since what there is fits on a page.
//!
//! Three things set it apart from KTX2, all from OpenGL's upload conventions:
//! - **Byte order.** The header's endianness word says whether the file was written on a
//!   big-endian machine. Then every header field, `imageSize` and key/value length is swapped,
//!   and so is the texel data, in units of `glTypeSize` (2 for a half or a 16-bit packed texel,
//!   4 for a float or a 32-bit packed one).
//! - **Row padding.** Uncompressed rows are padded to 4 bytes (`GL_UNPACK_ALIGNMENT`), so an RGB8
//!   row 5 texels wide takes 16 bytes, not 15. Block-compressed data has none. A level whose
//!   `imageSize` is exactly the unpadded size is read unpadded, as some writers store it.
//! - **Cubemap faces.** For a cubemap that isn't an array, `imageSize` is one face's size and each
//!   face is padded to 4 bytes on its own; for everything else it is the whole level.

use std::borrow::Cow;

use super::{Container, Dims, Orientation, from_vk, image, vk};
use crate::texture::{self, Format, Shape, decode_surface};
use crate::{Cx, DecodeError, DecodedImage, level_dims};

const C: Container = Container::V1;

/// The header's length: the identifier, then thirteen 32-bit fields.
const HEADER_LEN: usize = 64;

/// What the 64-byte header says.
struct Header {
    /// Written big-endian (or by anything else whose endianness differs from the file's
    /// reader): every 32-bit field, and the texel data in `gl_type_size` units, is byte-swapped.
    swapped: bool,
    gl_type: u32,
    gl_type_size: u32,
    gl_format: u32,
    gl_internal_format: u32,
    dims: Dims,
    /// Whether this is a cubemap that isn't an array: the one case whose `imageSize` is a face's.
    lone_cube: bool,
    kvd_len: u32,
}

fn header(bytes: &[u8]) -> Result<Header, DecodeError> {
    let head = bytes
        .get(..HEADER_LEN)
        .ok_or_else(|| C.malformed("the header is truncated"))?;
    let word = |i: usize| u32::from_le_bytes(head[12 + i * 4..16 + i * 4].try_into().unwrap());
    let swapped = match word(0) {
        0x0403_0201 => false,
        0x0102_0304 => true,
        other => {
            return Err(C.malformed(format_args!(
                "the endianness word is {other:#010x}, neither byte order's"
            )));
        }
    };
    let field = |i: usize| {
        if swapped {
            word(i).swap_bytes()
        } else {
            word(i)
        }
    };
    let gl_type_size = field(2);
    if !matches!(gl_type_size, 1 | 2 | 4) {
        return Err(C.malformed(format_args!("a glTypeSize of {gl_type_size}")));
    }
    let (elements, faces) = (field(9), field(10));
    let dims = Dims::new(C, field(6), field(7), field(8), elements, faces, field(11))?;
    Ok(Header {
        swapped,
        gl_type: field(1),
        gl_type_size,
        gl_format: field(3),
        gl_internal_format: field(4),
        dims,
        lone_cube: faces == 6 && elements == 0,
        kvd_len: field(12),
    })
}

/// The format the header's GL enums name, or why it can't be read.
fn identify(h: &Header) -> Result<Format, DecodeError> {
    let fmt = vk_for_gl(h.gl_type, h.gl_format, h.gl_internal_format)
        .and_then(|v| from_vk(v, C))
        .ok_or_else(|| {
            C.malformed(format_args!(
                "unsupported format: glInternalFormat {:#06x}, glFormat {:#06x}, glType {:#06x}",
                h.gl_internal_format, h.gl_format, h.gl_type
            ))
        })?;
    if fmt.power_of_two_only()
        && !(h.dims.width.is_power_of_two() && h.dims.height.is_power_of_two())
    {
        return Err(C.malformed(format_args!(
            "{} needs power-of-two dimensions, not {}x{}",
            fmt.label, h.dims.width, h.dims.height
        )));
    }
    Ok(fmt)
}

/// The `VkFormat` (or one of [`vk`]'s pseudo-formats for what Vulkan can't spell) that the GL
/// enums describe: by `glInternalFormat` for compressed data (`glType` 0), by `glFormat` and
/// `glType` for the rest. sRGB and linear spellings decode alike, so the unsized internal
/// formats need no table of their own.
fn vk_for_gl(gl_type: u32, gl_format: u32, internal: u32) -> Option<u32> {
    use self::vk::*;
    if gl_type == 0 {
        return Some(match internal {
            0x83F0 => BC1_RGB_UNORM_BLOCK,  // COMPRESSED_RGB_S3TC_DXT1
            0x8C4C => BC1_RGB_SRGB_BLOCK,   // COMPRESSED_SRGB_S3TC_DXT1
            0x83F1 => BC1_RGBA_UNORM_BLOCK, // COMPRESSED_RGBA_S3TC_DXT1
            0x8C4D => BC1_RGBA_SRGB_BLOCK,
            0x83F2 | 0x8C4E => BC2_UNORM_BLOCK, // DXT3
            0x83F3 | 0x8C4F => BC3_UNORM_BLOCK, // DXT5
            0x8DBB => BC4_UNORM_BLOCK,          // COMPRESSED_RED_RGTC1
            0x8DBC => BC4_SNORM_BLOCK,
            0x8DBD => BC5_UNORM_BLOCK, // COMPRESSED_RG_RGTC2
            0x8DBE => BC5_SNORM_BLOCK,
            0x8E8C | 0x8E8D => BC7_UNORM_BLOCK, // BPTC_UNORM, SRGB_ALPHA_BPTC_UNORM
            0x8E8E => BC6H_SFLOAT_BLOCK,        // RGB_BPTC_SIGNED_FLOAT
            0x8E8F => BC6H_UFLOAT_BLOCK,
            0x8D64 => ETC1, // ETC1_RGB8_OES
            0x9270 => EAC_R11_UNORM_BLOCK,
            0x9271 => EAC_R11_SNORM_BLOCK,
            0x9272 => EAC_R11G11_UNORM_BLOCK,
            0x9273 => EAC_R11G11_SNORM_BLOCK,
            0x9274 | 0x9275 => ETC2_R8G8B8_UNORM_BLOCK,
            0x9276 | 0x9277 => ETC2_R8G8B8A1_UNORM_BLOCK,
            0x9278 | 0x9279 => ETC2_R8G8B8A8_UNORM_BLOCK,
            // COMPRESSED_RGBA_ASTC_4x4 .. 12x12, then their sRGB spellings: the same 14
            // footprints in the order Vulkan's run takes them.
            0x93B0..=0x93BD => ASTC_4X4_UNORM_BLOCK + 2 * (internal - 0x93B0),
            0x93D0..=0x93DD => ASTC_4X4_UNORM_BLOCK + 2 * (internal - 0x93D0),
            0x8C00 | 0x8C02 | 0x8A55 | 0x8A57 => PVRTC1_4BPP_UNORM_BLOCK,
            0x8C01 | 0x8C03 | 0x8A54 | 0x8A56 => PVRTC1_2BPP_UNORM_BLOCK,
            _ => return None,
        });
    }
    const RED: u32 = 0x1903;
    const RG: u32 = 0x8227;
    const RGB: u32 = 0x1907;
    const RGBA: u32 = 0x1908;
    const BGR: u32 = 0x80E0;
    const BGRA: u32 = 0x80E1;
    const ALPHA: u32 = 0x1906;
    const LUMINANCE: u32 = 0x1909;
    const LUMINANCE_ALPHA: u32 = 0x190A;
    const UNSIGNED_BYTE: u32 = 0x1401;
    const UNSIGNED_SHORT: u32 = 0x1403;
    const HALF_FLOAT: u32 = 0x140B;
    const HALF_FLOAT_OES: u32 = 0x8D61;
    const FLOAT: u32 = 0x1406;
    Some(match (gl_format, gl_type) {
        (RED, UNSIGNED_BYTE) => R8_UNORM,
        (RG, UNSIGNED_BYTE) => R8G8_UNORM,
        (RGB, UNSIGNED_BYTE) => R8G8B8_UNORM,
        (RGBA, UNSIGNED_BYTE) => R8G8B8A8_UNORM,
        (BGR, UNSIGNED_BYTE) => B8G8R8_UNORM,
        (BGRA, UNSIGNED_BYTE) => B8G8R8A8_UNORM,
        (ALPHA, UNSIGNED_BYTE) => A8_UNORM,
        (LUMINANCE, UNSIGNED_BYTE) => L8,
        (LUMINANCE_ALPHA, UNSIGNED_BYTE) => L8A8,
        (RED, UNSIGNED_SHORT) => R16_UNORM,
        (RG, UNSIGNED_SHORT) => R16G16_UNORM,
        (RGB, UNSIGNED_SHORT) => R16G16B16_UNORM,
        (RGBA, UNSIGNED_SHORT) => R16G16B16A16_UNORM,
        (LUMINANCE, UNSIGNED_SHORT) => L16,
        (LUMINANCE_ALPHA, UNSIGNED_SHORT) => L16A16,
        (RED, HALF_FLOAT | HALF_FLOAT_OES) => R16_SFLOAT,
        (RG, HALF_FLOAT | HALF_FLOAT_OES) => R16G16_SFLOAT,
        (RGB, HALF_FLOAT | HALF_FLOAT_OES) => R16G16B16_SFLOAT,
        (RGBA, HALF_FLOAT | HALF_FLOAT_OES) => R16G16B16A16_SFLOAT,
        (RED, FLOAT) => R32_SFLOAT,
        (RG, FLOAT) => R32G32_SFLOAT,
        (RGB, FLOAT) => R32G32B32_SFLOAT,
        (RGBA, FLOAT) => R32G32B32A32_SFLOAT,
        // The packed types, read as GL defines them: the first component in the most
        // significant bits, or with `_REV` in the least.
        (RGB, 0x8363) => R5G6B5_UNORM_PACK16, // UNSIGNED_SHORT_5_6_5
        (RGBA, 0x8033) => R4G4B4A4_UNORM_PACK16, // UNSIGNED_SHORT_4_4_4_4
        (RGBA, 0x8034) => R5G5B5A1_UNORM_PACK16, // UNSIGNED_SHORT_5_5_5_1
        (BGRA, 0x8365) => A4R4G4B4_UNORM_PACK16, // UNSIGNED_SHORT_4_4_4_4_REV
        (BGRA, 0x8366) => A1R5G5B5_UNORM_PACK16, // UNSIGNED_SHORT_1_5_5_5_REV
        (RGBA, 0x8367) => A8B8G8R8_UNORM_PACK32, // UNSIGNED_INT_8_8_8_8_REV
        (BGRA, 0x8367) => B8G8R8A8_UNORM,     // the same, blue first
        (RGBA, 0x8368) => A2B10G10R10_UNORM_PACK32, // UNSIGNED_INT_2_10_10_10_REV
        (RGB, 0x8C3B) => B10G11R11_UFLOAT_PACK32, // UNSIGNED_INT_10F_11F_11F_REV
        (RGB, 0x8C3E) => E5B9G9R9_UFLOAT_PACK32, // UNSIGNED_INT_5_9_9_9_REV
        _ => return None,
    })
}

/// Where one level sits, and how its rows are laid out.
struct Level {
    /// The first byte of its data, just past its `imageSize`.
    start: usize,
    /// Its `imageSize`: one face's size for a lone cubemap, the whole level's otherwise.
    image_size: usize,
    /// Bytes from one row to the next: the row, padded to 4 bytes for uncompressed data unless
    /// the level is stored without.
    row_stride: usize,
}

/// The levels as stored, walked from their `imageSize` fields. A level whose `imageSize` or data
/// runs past the end of the file ends the walk; level 0 doing so is an error.
fn levels(bytes: &[u8], h: &Header, fmt: &Format) -> Result<Vec<Level>, DecodeError> {
    let read_u32 = |at: usize| {
        let w = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        Some(if h.swapped { w.swap_bytes() } else { w })
    };
    let d = &h.dims;
    let surfaces = d.surfaces(C)? as usize;
    let compressed = h.gl_type == 0;
    let mut out = Vec::new();
    let mut at = HEADER_LEN
        .checked_add(h.kvd_len as usize)
        .filter(|&a| a <= bytes.len())
        .ok_or_else(|| C.malformed("the key/value data runs past the end of the file"))?;
    for level in 0..d.levels {
        let (lw, lh) = level_dims(d.width, d.height, level);
        let ld = (d.depth >> level).max(1) as usize;
        let shape = || -> Option<(usize, usize, usize)> {
            let row = fmt.level_bytes(lw, 1, 1)?;
            let tight = fmt.level_bytes(lw, lh, 1)?;
            let padded = if compressed {
                tight
            } else {
                row.next_multiple_of(4).checked_mul(lh as usize)?
            };
            Some((row, tight, padded))
        };
        let Some((row, tight, padded)) = shape() else {
            return Err(DecodeError::TooLarge(format!(
                "KTX {}x{} level {level} overflows a usize",
                d.width, d.height
            )));
        };
        // What one `imageSize` covers: a face of a lone cubemap, or every surface of the level.
        let per_size = if h.lone_cube { 1 } else { surfaces * ld };
        let Some(image_size) = read_u32(at).map(|s| s as usize) else {
            if level == 0 {
                return Err(C.malformed("level 0 is missing"));
            }
            break;
        };
        let start = at + 4;
        let (row_stride, needed) = if compressed {
            (0, tight.checked_mul(per_size))
        } else if Some(image_size) == tight.checked_mul(per_size) {
            // Stored without row padding (or it needs none).
            (row, Some(image_size))
        } else {
            (row.next_multiple_of(4), padded.checked_mul(per_size))
        };
        // The level's bytes in the file: six padded faces, or one block.
        let span = if h.lone_cube {
            image_size.next_multiple_of(4).checked_mul(6)
        } else {
            Some(image_size)
        };
        let fits = needed.is_some_and(|n| image_size >= n)
            && span
                .and_then(|s| start.checked_add(s))
                .is_some_and(|end| end <= bytes.len());
        if !fits {
            if level == 0 {
                return Err(C.malformed(format_args!(
                    "level 0 holds {image_size} bytes; its {}x{} header needs more",
                    d.width, d.height
                )));
            }
            break;
        }
        out.push(Level {
            start,
            image_size,
            row_stride,
        });
        // Past the level, then to the next multiple of 4 (`mipPadding`).
        at = (start + span.unwrap_or(0)).next_multiple_of(4);
    }
    Ok(out)
}

/// The bytes of surface `frame` of a level, `fw`x`fh`, as the codec reads them: unpadded and in
/// the host's byte order. Borrowed from the file when it already is.
fn surface<'a>(
    bytes: &'a [u8],
    h: &Header,
    fmt: &Format,
    level: &Level,
    frame: u32,
    fw: u32,
    fh: u32,
) -> Option<Cow<'a, [u8]>> {
    let tight = fmt.level_bytes(fw, fh, 1)?;
    let compressed = h.gl_type == 0;
    // Block-compressed data is stored as it is read: no row padding, and no byte order.
    let (row, stride) = if compressed {
        (tight, tight)
    } else {
        (fmt.level_bytes(fw, 1, 1)?, level.row_stride)
    };
    let stored = if compressed {
        tight
    } else {
        stride.checked_mul(fh as usize)?
    };
    let start = if h.lone_cube {
        level
            .start
            .checked_add((frame as usize).checked_mul(level.image_size.next_multiple_of(4))?)?
    } else {
        level
            .start
            .checked_add((frame as usize).checked_mul(stored)?)?
    };
    let src = bytes.get(start..start.checked_add(stored)?)?;
    let swap = h.swapped && h.gl_type_size > 1 && !compressed;
    if stride == row && !swap {
        return Some(Cow::Borrowed(src));
    }
    let mut out = Vec::with_capacity(tight);
    for r in src.chunks(stride.max(1)) {
        out.extend_from_slice(r.get(..row)?);
    }
    if swap {
        for unit in out.chunks_exact_mut(h.gl_type_size as usize) {
            unit.reverse();
        }
    }
    Some(Cow::Owned(out))
}

/// The file's `KTXorientation` (`S=r,T=d`, perhaps with `,R=i`), or the default (right, down).
fn orientation(bytes: &[u8], h: &Header) -> Orientation {
    let mut kvd = bytes
        .get(HEADER_LEN..HEADER_LEN.saturating_add(h.kvd_len as usize))
        .unwrap_or_default();
    while let Some(len) = kvd.get(..4).map(|b| {
        let w = u32::from_le_bytes(b.try_into().unwrap());
        (if h.swapped { w.swap_bytes() } else { w }) as usize
    }) {
        let Some(entry) = len.checked_add(4).and_then(|end| kvd.get(4..end)) else {
            break;
        };
        if let Some(value) = entry.strip_prefix(b"KTXorientation\0") {
            let axis = |name: &[u8]| value.windows(3).find(|w| &w[..2] == name).map(|w| w[2]);
            return Orientation {
                flip_x: axis(b"S=") == Some(b'l'),
                flip_y: axis(b"T=") == Some(b'u'),
            };
        }
        // Each entry is padded to a multiple of 4 bytes.
        let next = 4usize.saturating_add(len.next_multiple_of(4));
        kvd = kvd.get(next..).unwrap_or_default();
    }
    Orientation::default()
}

/// Decode a KTX from memory, normalized to RGBA, as [`texture::assemble`] lays it out.
pub(crate) fn decode(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let h = header(bytes)?;
    let fmt = identify(&h)?;
    let d = &h.dims;
    // The guard runs on the header's bare claim first, before a single `imageSize` is read.
    let all = d.layout();
    texture::check_texture(cx, fmt.out, d.width, d.height, all, "KTX")?;

    let levels = levels(bytes, &h, &fmt)?;
    let orientation = orientation(bytes, &h);
    let shape = Shape {
        width: d.width,
        height: d.height,
        levels: levels.len() as u32,
        all,
    };
    let bpp = fmt.out.bytes_per_pixel();
    let img = texture::assemble(
        cx,
        fmt.out,
        &shape,
        "KTX",
        texture::Alpha::Straight,
        |level, frame, fw, fh| {
            let src = surface(bytes, &h, &fmt, levels.get(level as usize)?, frame, fw, fh)?;
            let mut cell = decode_surface(&fmt, &src, fw, fh);
            orientation.apply(&mut cell, fw, fh, bpp);
            Some(cell)
        },
    )?;
    Ok(image(
        img,
        crate::Format::Ktx,
        fmt.out,
        fmt.label,
        fmt.channels,
        fmt.bit_depth,
    ))
}

/// The size [`decode`] returns with [`Surfaces::Sheet`](crate::Surfaces::Sheet), from the 64-byte
/// header alone (FD19).
pub(crate) fn probe_dims(head: &[u8]) -> Result<(u32, u32), DecodeError> {
    let h = header(head)?;
    identify(&h)?;
    let (cols, rows) = h.dims.layout().map_or((1, 1), |l| (l.cols, l.rows));
    let too_large = || texture::too_large("KTX");
    Ok((
        h.dims.width.checked_mul(cols).ok_or_else(too_large)?,
        h.dims.height.checked_mul(rows).ok_or_else(too_large)?,
    ))
}
