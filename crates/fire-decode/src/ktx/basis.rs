//! Basis Universal in KTX2 — glTF's `KHR_texture_basisu` — transcoded by `basisu`, a pure-Rust
//! port of the reference transcoder that matches it byte for byte.
//!
//! A Basis texture is stored in a codec meant to be transcoded to whatever block format the GPU
//! takes. Here the target is uncompressed: RGBA8 for the LDR codecs (ETC1S, which BasisLZ
//! supercompresses; UASTC, often under Zstandard), and half-float RGBA for the HDR ones (UASTC
//! HDR 4x4, ASTC HDR 6x6), so they reach the exposure and tonemap controls.
//!
//! The transcoder sizes a Zstandard level's buffer from the length the file declares for it. So
//! that number is bounded here first, by the most a level of the header's dimensions can hold
//! (UASTC's 16 bytes for every 4x4 block, the densest Basis codec) and by the setup's
//! [`Limits`](crate::Limits): a failed allocation aborts the process, and no `catch_unwind`
//! stands between it and a crafted file.

use basisu::{DecodeFlags, SourceFormat, TargetFormat, Transcoder};
use ktx2::{Header, Reader, SupercompressionScheme};

use super::Dims;
use super::image;
use super::v2::orientation;
use crate::texture::{self, Shape};
use crate::{Cx, DecodeError, DecodedImage, PixelFormat, level_dims};

fn malformed(what: impl std::fmt::Display) -> DecodeError {
    DecodeError::Malformed(format!("KTX2 Basis Universal: {what}"))
}

/// Decode a Basis Universal KTX2. `header` and `dims` are [`super::v2`]'s reading of it.
pub(super) fn decode(
    bytes: &[u8],
    header: &Header,
    dims: &Dims,
    cx: &Cx,
) -> Result<DecodedImage, DecodeError> {
    if dims.depth > 1 {
        return Err(malformed("a volume texture; Basis textures are 2D"));
    }
    // The guard on the header's claim, at the narrower output; an HDR codec is guarded again at
    // its own width once the transcoder has said which it is.
    let all = dims.layout();
    texture::check_texture(
        cx,
        PixelFormat::Rgba8Unorm,
        dims.width,
        dims.height,
        all,
        "KTX2",
    )?;

    let reader = Reader::new(bytes).map_err(malformed)?;
    let levels = bounded_levels(&reader, header, dims, cx)?;

    let t = Transcoder::new(bytes).map_err(|e| match header.format {
        // An ASTC HDR file the transcoder won't take: plain ASTC HDR 4x4, not UASTC HDR.
        Some(f) => DecodeError::Malformed(format!(
            "KTX2: unsupported format {}; only Basis Universal's encodings of it are read",
            super::describe_vk(f.value())
        )),
        None => malformed(format_args!("{e:?}")),
    })?;
    if t.is_video() {
        return Err(malformed(
            "an ETC1S video, whose frames decode only in sequence",
        ));
    }
    let (target, out, label, bit_depth) = match t.source_format() {
        SourceFormat::Etc1s => (
            TargetFormat::Rgba32,
            PixelFormat::Rgba8Unorm,
            "KTX2 ETC1S",
            8,
        ),
        SourceFormat::UastcLdr => (
            TargetFormat::Rgba32,
            PixelFormat::Rgba8Unorm,
            "KTX2 UASTC",
            8,
        ),
        SourceFormat::XuastcLdr(_) => (
            TargetFormat::Rgba32,
            PixelFormat::Rgba8Unorm,
            "KTX2 XUASTC",
            8,
        ),
        SourceFormat::AstcLdr(_) => (
            TargetFormat::Rgba32,
            PixelFormat::Rgba8Unorm,
            "KTX2 ASTC",
            8,
        ),
        SourceFormat::UastcHdr4x4 => (
            TargetFormat::RgbaHalf,
            PixelFormat::Rgba16Float,
            "KTX2 UASTC HDR",
            16,
        ),
        SourceFormat::AstcHdr6x6 => (
            TargetFormat::RgbaHalf,
            PixelFormat::Rgba16Float,
            "KTX2 ASTC HDR 6x6",
            16,
        ),
        SourceFormat::UastcHdr6x6 => (
            TargetFormat::RgbaHalf,
            PixelFormat::Rgba16Float,
            "KTX2 UASTC HDR 6x6",
            16,
        ),
        other => return Err(malformed(format_args!("the {other:?} codec"))),
    };
    if out != PixelFormat::Rgba8Unorm {
        texture::check_texture(cx, out, dims.width, dims.height, all, "KTX2")?;
    }

    let faces = t.face_count().max(1);
    let shape = Shape {
        width: dims.width,
        height: dims.height,
        levels: levels.min(t.level_count()),
        all,
    };
    let orientation = orientation(&reader);
    let bpp = out.bytes_per_pixel();
    let img = texture::assemble(
        cx,
        out,
        &shape,
        "KTX2",
        super::v2::alpha(&reader),
        |level, frame, fw, fh| {
            let mut cell = t
                .transcode_image(
                    level,
                    frame / faces,
                    frame % faces,
                    target,
                    DecodeFlags::NONE,
                )
                .ok()?;
            // The half-float target is little-endian halves; the canvas holds native ones.
            if cfg!(target_endian = "big") && out == PixelFormat::Rgba16Float {
                for h in cell.as_chunks_mut::<2>().0 {
                    h.reverse();
                }
            }
            orientation.apply(&mut cell, fw, fh, bpp);
            Some(cell)
        },
    )?;
    let channels = if t.has_alpha() { 4 } else { 3 };
    Ok(image(
        img,
        crate::Format::Ktx2,
        out,
        label,
        channels,
        bit_depth,
    ))
}

/// How many leading levels can be handed to the transcoder: each one's declared inflated length
/// no more than its dimensions allow and within the setup's limits. Level 0 failing is an error;
/// a mip failing ends the chain there.
fn bounded_levels(
    reader: &Reader<&[u8]>,
    header: &Header,
    dims: &Dims,
    cx: &Cx,
) -> Result<u32, DecodeError> {
    if header.supercompression_scheme != Some(SupercompressionScheme::Zstandard) {
        return Ok(dims.levels);
    }
    let surfaces = u64::from(dims.layers) * u64::from(dims.faces);
    let max_bytes = cx.limits().max_bytes;
    for (level, l) in reader.levels().take(dims.levels as usize).enumerate() {
        let (lw, lh) = level_dims(dims.width, dims.height, level as u32);
        let most = u64::from(lw.div_ceil(4)) * u64::from(lh.div_ceil(4)) * 16 * surfaces;
        let declared = l.uncompressed_byte_length;
        let refusal = if declared > most {
            Some(malformed(format_args!(
                "level {level} says it inflates to {declared} bytes; a {lw}x{lh} level holds \
                 at most {most}"
            )))
        } else if declared > max_bytes {
            Some(DecodeError::TooLarge(format!(
                "KTX2 level {level} inflates to {declared} bytes, past the {max_bytes}-byte \
                 decode guard"
            )))
        } else {
            None
        };
        match refusal {
            Some(e) if level == 0 => return Err(e),
            Some(_) => return Ok(level as u32),
            None => {}
        }
    }
    Ok(dims.levels)
}
