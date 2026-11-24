//! KTX2: an 80-byte header, a level index, a data format descriptor (DFD), key/value data, and
//! each mip level's surfaces — every layer, then every face, then every depth slice — optionally
//! supercompressed. `ktx2` parses the header and the index; Basis Universal payloads (a
//! `VK_FORMAT_UNDEFINED` file, or UASTC HDR) go to [`super::basis`].
//!
//! Each level is one block of bytes, inflated whole when it is supercompressed. Before that the
//! level's declared inflated length has to be exactly what the header's dimensions imply, and
//! within the setup's [`Limits`](crate::Limits): a crafted header can claim terabytes, and a
//! failed allocation aborts rather than panics.

use std::io::Read;

use ktx2::{Header, Reader, SupercompressionScheme};

use super::{Container, Orientation, describe_vk, from_vk, image, layout, vk};
use crate::texture::{self, Format, Shape, decode_surface};
use crate::{Cx, DecodeError, DecodedImage, SheetLayout, level_dims};

/// The shape a KTX2 header gives, with its zeros made ones.
pub(super) struct Dims {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) depth: u32,
    pub(super) layers: u32,
    pub(super) faces: u32,
    /// The levels stored, at least 1 and never more than the dimensions can halve to.
    pub(super) levels: u32,
}

impl Dims {
    /// How the surfaces tile into a sheet, or `None` for one surface.
    pub(super) fn layout(&self) -> Option<SheetLayout> {
        layout(self.depth, self.layers, self.faces)
    }
}

fn malformed(what: impl std::fmt::Display) -> DecodeError {
    DecodeError::Malformed(format!("KTX2: {what}"))
}

/// The header, from the file's first 80 bytes.
pub(super) fn header(bytes: &[u8]) -> Result<Header, DecodeError> {
    let head: &[u8; Header::LENGTH] = bytes
        .get(..Header::LENGTH)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| malformed("the header is truncated"))?;
    Header::from_bytes(head).map_err(malformed)
}

/// The header's shape, checked for what KTX2 allows: one or six faces, square cubemap faces with
/// no depth, and no arrays of volumes (which Vulkan has no image type for either).
pub(super) fn dims(h: &Header) -> Result<Dims, DecodeError> {
    let (width, height, depth) = (h.pixel_width, h.pixel_height.max(1), h.pixel_depth.max(1));
    let (layers, faces) = (h.layer_count.max(1), h.face_count);
    if faces != 1 && faces != 6 {
        return Err(malformed(format_args!("a texture with {faces} faces")));
    }
    if faces == 6 && (width != height || depth > 1) {
        return Err(malformed(format_args!(
            "a {width}x{height}x{depth} cubemap; its faces must be square and flat"
        )));
    }
    if depth > 1 && layers > 1 {
        return Err(malformed("an array of volume textures"));
    }
    // A header may claim more levels than the dimensions can have, and a corrupt one may claim
    // billions. Either way the chain stops where the halving reaches 1x1x1.
    let max_levels = 32 - width.max(height).max(depth).leading_zeros();
    let levels = h.level_count.clamp(1, max_levels);
    Ok(Dims {
        width,
        height,
        depth,
        layers,
        faces,
        levels,
    })
}

/// Whether `vk` is one of the two ASTC HDR formats Basis Universal's transcoder reads: 4x4, as
/// UASTC HDR, and 6x6. The other HDR footprints have no decoder here and are refused by name.
fn astc_hdr(vk: u32) -> bool {
    vk == vk::ASTC_4X4_SFLOAT_BLOCK || vk == vk::ASTC_6X6_SFLOAT_BLOCK
}

/// The non-Basis format `vk` names, or why it can't be read.
fn identify(vk: u32, dims: &Dims) -> Result<Format, DecodeError> {
    // The table's pseudo-formats (above 2^31) are KTX v1's spellings, not values a KTX2 can name.
    let fmt = Some(vk)
        .filter(|v| v & 0x8000_0000 == 0)
        .and_then(|v| from_vk(v, Container::V2))
        .ok_or_else(|| malformed(format_args!("unsupported format {}", describe_vk(vk))))?;
    if fmt.power_of_two_only() && !(dims.width.is_power_of_two() && dims.height.is_power_of_two()) {
        return Err(malformed(format_args!(
            "{} needs power-of-two dimensions, not {}x{}",
            fmt.label, dims.width, dims.height
        )));
    }
    Ok(fmt)
}

/// Decode a KTX2 from memory, normalized to RGBA, as [`texture::assemble`] lays it out.
pub(crate) fn decode(bytes: &[u8], cx: &Cx) -> Result<DecodedImage, DecodeError> {
    let header = header(bytes)?;
    let dims = dims(&header)?;
    let vk = match header.format {
        None => return super::basis::decode(bytes, &header, &dims, cx),
        Some(f) if astc_hdr(f.value()) => return super::basis::decode(bytes, &header, &dims, cx),
        Some(f) => f.value(),
    };
    let fmt = identify(vk, &dims)?;
    let scheme = header.supercompression_scheme;
    match scheme {
        None | Some(SupercompressionScheme::Zstandard) | Some(SupercompressionScheme::ZLIB) => {}
        Some(SupercompressionScheme::BasisLZ) => {
            return Err(malformed(format_args!(
                "BasisLZ over {}; it is Basis Universal's, with VK_FORMAT_UNDEFINED",
                describe_vk(vk)
            )));
        }
        Some(other) => {
            return Err(malformed(format_args!(
                "unknown supercompression scheme {}",
                other.value()
            )));
        }
    }

    // The guard runs on the header's bare claim first: one surface, and the sheet its layers,
    // faces or slices tile into. Nothing past the header has been read.
    let all = dims.layout();
    texture::check_texture(cx, fmt.out, dims.width, dims.height, all, "KTX2")?;

    let reader = Reader::new(bytes).map_err(malformed)?;
    let mut levels = Levels::new(&reader, &fmt, &dims, scheme, cx)?;
    let orientation = orientation(&reader);
    let shape = Shape {
        width: dims.width,
        height: dims.height,
        levels: levels.count,
        all,
    };
    let bpp = fmt.out.bytes_per_pixel();
    let img = texture::assemble(
        cx,
        fmt.out,
        &shape,
        "KTX2",
        alpha(&reader),
        |level, frame, fw, fh| {
            // Within a level, surface `frame` — layer-major, then face, then depth slice — is
            // the `frame`th of equal-sized runs.
            let slice = fmt.level_bytes(fw, fh, 1)?;
            let start = (frame as usize).checked_mul(slice)?;
            let src = levels.get(level)?.get(start..start.checked_add(slice)?)?;
            let mut cell = decode_surface(&fmt, src, fw, fh);
            orientation.apply(&mut cell, fw, fh, bpp);
            Some(cell)
        },
    )?;
    Ok(image(
        img,
        crate::Format::Ktx2,
        fmt.out,
        fmt.label,
        fmt.channels,
        fmt.bit_depth,
    ))
}

/// How the file's colour relates to its alpha, from its data format descriptor: premultiplied or
/// not, and if premultiplied, in linear light when the transfer function is sRGB.
pub(super) fn alpha(reader: &Reader<&[u8]>) -> texture::Alpha {
    if reader.is_alpha_premultiplied() != Some(true) {
        texture::Alpha::Straight
    } else if reader.transfer_function() == Some(ktx2::TransferFunction::SRGB) {
        texture::Alpha::PremultipliedInLinearLight
    } else {
        texture::Alpha::Premultiplied
    }
}

/// The file's `KTXorientation`, or the default (right, down).
pub(super) fn orientation(reader: &Reader<&[u8]>) -> Orientation {
    reader
        .key_value_data()
        .find(|(k, _)| *k == "KTXorientation")
        .map_or_else(Orientation::default, |(_, v)| Orientation::from_ktx2(v))
}

/// The size [`decode`] returns with [`Surfaces::Sheet`](crate::Surfaces::Sheet), from the 80-byte
/// header alone: one surface, or the sheet its layers, faces or slices tile into (FD19). Decode
/// reads level 0 strictly, so for every file that decodes the two agree.
pub(crate) fn probe_dims(head: &[u8]) -> Result<(u32, u32), DecodeError> {
    let header = header(head)?;
    let dims = dims(&header)?;
    if let Some(f) = header.format
        && !astc_hdr(f.value())
    {
        identify(f.value(), &dims)?;
    }
    let (cols, rows) = dims.layout().map_or((1, 1), |l| (l.cols, l.rows));
    let too_large = || texture::too_large("KTX2");
    Ok((
        dims.width.checked_mul(cols).ok_or_else(too_large)?,
        dims.height.checked_mul(rows).ok_or_else(too_large)?,
    ))
}

/// A KTX2's levels, as the bytes their surfaces are read from: as stored, or inflated — one level
/// at a time, the latest kept, since [`texture::assemble`] reads every surface of a level before
/// it moves to the next.
struct Levels<'a> {
    stored: Vec<ktx2::Level<'a>>,
    scheme: Option<SupercompressionScheme>,
    /// Each level's size once inflated: what its dimensions imply, times layers and faces.
    expected: Vec<usize>,
    max_bytes: u64,
    inflated: Option<(u32, Vec<u8>)>,
    count: u32,
}

impl<'a> Levels<'a> {
    fn new(
        reader: &'a Reader<&'a [u8]>,
        fmt: &Format,
        dims: &Dims,
        scheme: Option<SupercompressionScheme>,
        cx: &Cx,
    ) -> Result<Self, DecodeError> {
        let stored: Vec<_> = reader.levels().take(dims.levels as usize).collect();
        let surfaces = dims.layers.checked_mul(dims.faces).ok_or_else(|| {
            DecodeError::TooLarge(format!(
                "KTX2 {} layers of {} faces overflow",
                dims.layers, dims.faces
            ))
        })?;
        let expected = (0..stored.len() as u32)
            .map(|level| {
                let (lw, lh) = level_dims(dims.width, dims.height, level);
                let ld = (dims.depth >> level).max(1);
                fmt.level_bytes(lw, lh, ld)
                    .and_then(|n| n.checked_mul(surfaces as usize))
                    .ok_or_else(|| {
                        DecodeError::TooLarge(format!(
                            "KTX2 {}x{} level {level} overflows a usize",
                            dims.width, dims.height
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut levels = Levels {
            count: stored.len() as u32,
            stored,
            scheme,
            expected,
            max_bytes: cx.limits().max_bytes,
            inflated: None,
        };

        // Level 0 is required, so it is checked here, where a failure can say why; a mip that
        // fails the same checks only ends the chain.
        if let (Some(_), Some(&want), Some(level)) = (
            levels.scheme,
            levels.expected.first(),
            levels.stored.first(),
        ) {
            if want as u64 > levels.max_bytes {
                return Err(DecodeError::TooLarge(format!(
                    "KTX2 level 0 inflates to {want} bytes, past the {}-byte decode guard",
                    levels.max_bytes
                )));
            }
            if level.uncompressed_byte_length != want as u64 {
                return Err(malformed(format_args!(
                    "level 0 says it inflates to {} bytes; its {}x{} header describes {want}",
                    level.uncompressed_byte_length, dims.width, dims.height
                )));
            }
            if levels.get(0).is_none() {
                return Err(malformed(format_args!(
                    "level 0 doesn't inflate to the {want} bytes it declares"
                )));
            }
        }
        Ok(levels)
    }

    /// The bytes of `level`, exactly as long as its surfaces need: `None` when they are short,
    /// or when an inflated level isn't the length its header implies.
    fn get(&mut self, level: u32) -> Option<&[u8]> {
        let i = level as usize;
        let (stored, want) = (self.stored.get(i)?, *self.expected.get(i)?);
        let Some(scheme) = self.scheme else {
            return stored.data.get(..want);
        };
        if self.inflated.as_ref().is_none_or(|(l, _)| *l != level) {
            self.inflated = None;
            if want as u64 > self.max_bytes || stored.uncompressed_byte_length != want as u64 {
                return None;
            }
            self.inflated = Some((level, inflate(stored.data, scheme, want)?));
        }
        self.inflated.as_ref().map(|(_, b)| b.as_slice())
    }
}

/// Inflate one supercompressed level to exactly `want` bytes, or `None`. The output is capped one
/// byte past `want`, so a stream that keeps going is caught without being followed.
fn inflate(data: &[u8], scheme: SupercompressionScheme, want: usize) -> Option<Vec<u8>> {
    let out = match scheme {
        SupercompressionScheme::Zstandard => {
            let mut dec = ruzstd::StreamingDecoder::new(data).ok()?;
            let mut out = Vec::with_capacity(want);
            dec.by_ref()
                .take(want as u64 + 1)
                .read_to_end(&mut out)
                .ok()?;
            out
        }
        SupercompressionScheme::ZLIB => {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, want).ok()?
        }
        _ => return None,
    };
    (out.len() == want).then_some(out)
}
