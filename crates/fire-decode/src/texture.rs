//! The texture-container core that DDS ([`crate::dds`]) and KTX / KTX2 share: the payload codecs,
//! the sheet several surfaces are tiled into, and the file's own mip chain.
//!
//! A texture container is a header followed by raw surface data in one of a hundred GPU layouts.
//! The containers differ in their headers: how a layout is named (a DXGI value, a FourCC, bit
//! masks, a `VkFormat`, a GL enum) and where each surface sits. Everything after that is the same.
//! So each container module maps its header onto a [`Format`] and a way to reach one surface's
//! bytes, and [`assemble`] does the rest.
//!
//! **The payload is decompressed, not uploaded compressed.** sokol_gfx can take BC1-BC7 blocks
//! straight to the GPU, and that would be exact and cheap — but [`DecodedImage`] is an
//! *uncompressed RGBA canvas* by contract, and everything downstream reads it as one: the mip
//! builder, the downscale guard, the alpha scan, the flipbook grid detector. Teaching all of
//! them about block payloads to save tens of milliseconds on a decode that already runs off the
//! UI thread is a bad trade, so the blocks are expanded here and the rest of the viewer never
//! learns texture containers exist.
//!
//! [`DecodedImage`]: crate::DecodedImage

use crate::{Cx, DecodeError, PixelFormat, SheetKind, SheetLayout, Surfaces, level_dims};

/// Half-precision 1.0, the alpha filled in for the alpha-less HDR formats.
pub(crate) const HALF_ONE: u16 = 0x3c00;

// --- format identification -----------------------------------------------------------------

/// A block-compressed payload. The `bool` is "the channels are signed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Block {
    Bc1,
    Bc2,
    Bc3,
    Bc4(bool),
    Bc5(bool),
    Bc6h(bool),
    Bc7,
}

impl Block {
    /// Bytes per 4x4 block. BC1 and BC4 carry one endpoint pair; the rest carry two.
    fn block_bytes(self) -> usize {
        match self {
            Block::Bc1 | Block::Bc4(_) => 8,
            _ => 16,
        }
    }
}

/// An uncompressed payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plain {
    /// Integer channels carved out of a `bytes`-wide little-endian unit by the header's R/G/B/A
    /// bit masks. This one arm covers every legacy D3D9 layout — `A8R8G8B8`, `X8B8G8R8`,
    /// `R5G6B5`, `A2R10G10B10`, `L8`, `A8` — plus whatever masks an exporter invents, because it
    /// reads the masks rather than matching a table of known ones. `luminance` replicates the red
    /// channel across green and blue (a grey or single-channel source).
    Masked {
        bytes: u8,
        masks: [u32; 4],
        luminance: bool,
    },
    /// `n` unsigned 16-bit channels in R, G, B, A order.
    U16(u8),
    /// `n` half-float channels.
    F16(u8),
    /// `n` 32-bit float channels.
    F32(u8),
    /// Three unsigned floats packed 11/11/10 into one 32-bit word.
    R11G11B10,
    /// Three 9-bit mantissas sharing one 5-bit exponent, packed into 32 bits.
    R9G9B9E5,
}

impl Plain {
    /// Bytes per texel in the file.
    pub(crate) fn texel_bytes(self) -> usize {
        match self {
            Plain::Masked { bytes, .. } => bytes as usize,
            Plain::U16(n) | Plain::F16(n) => 2 * n as usize,
            Plain::F32(n) => 4 * n as usize,
            Plain::R11G11B10 | Plain::R9G9B9E5 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Block(Block),
    Plain(Plain),
}

/// How the file stores its texels, and what they become.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Format {
    pub(crate) codec: Codec,
    /// The [`DecodedImage`](crate::DecodedImage) layout this decodes to.
    pub(crate) out: PixelFormat,
    /// Bits per channel of the source, for the status bar.
    pub(crate) bit_depth: u8,
    /// The source's meaningful channel count. Two-channel sources (BC5, R16G16) report 3: they
    /// are displayed as RGB with a zero blue lane, and calling them "Gray+A" would be a lie.
    pub(crate) channels: u8,
    /// The status-bar name for the encoding.
    pub(crate) label: &'static str,
}

impl Format {
    pub(crate) fn block(
        b: Block,
        out: PixelFormat,
        bit_depth: u8,
        channels: u8,
        label: &'static str,
    ) -> Self {
        Format {
            codec: Codec::Block(b),
            out,
            bit_depth,
            channels,
            label,
        }
    }

    pub(crate) fn plain(
        p: Plain,
        out: PixelFormat,
        bit_depth: u8,
        channels: u8,
        label: &'static str,
    ) -> Self {
        Format {
            codec: Codec::Plain(p),
            out,
            bit_depth,
            channels,
            label,
        }
    }

    /// Bytes one `w`x`h`x`d` surface occupies in the file, or `None` if that overflows.
    pub(crate) fn level_bytes(&self, w: u32, h: u32, d: u32) -> Option<usize> {
        let (w, h, d) = (w as usize, h as usize, d as usize);
        match self.codec {
            // Block formats round up to whole 4x4 blocks, so a 5x3 level still costs 2x1 blocks.
            Codec::Block(b) => w
                .div_ceil(4)
                .checked_mul(h.div_ceil(4))?
                .checked_mul(b.block_bytes())?
                .checked_mul(d),
            Codec::Plain(p) => w
                .checked_mul(h)?
                .checked_mul(p.texel_bytes())?
                .checked_mul(d),
        }
    }
}

/// Build a masked format, choosing the output depth from the widest channel: a 10- or 16-bit
/// lane keeps its precision in `Rgba16Unorm` rather than being crushed to 8.
pub(crate) fn masked(bytes: u8, masks: [u32; 4], luminance: bool, label: &'static str) -> Format {
    let widest = masks.iter().copied().map(mask_width).max().unwrap_or(0);
    let out = if widest > 8 {
        PixelFormat::Rgba16Unorm
    } else {
        PixelFormat::Rgba8Unorm
    };
    // One colour lane (or an explicit luminance flag) means grey: replicate red across green and
    // blue rather than showing an R8 height map as a red one.
    let colour = u8::from(masks[0] != 0) + u8::from(masks[1] != 0) + u8::from(masks[2] != 0);
    let grey = luminance || colour <= 1;
    let has_alpha = masks[3] != 0;
    let channels = match (grey, has_alpha) {
        (true, true) => 2,
        (true, false) => 1,
        (false, true) => 4,
        (false, false) => 3,
    };
    Format::plain(
        Plain::Masked {
            bytes,
            masks,
            luminance: grey,
        },
        out,
        widest.max(1) as u8,
        channels,
        label,
    )
}

/// Bits in a (contiguous) channel mask.
pub(crate) fn mask_width(m: u32) -> u32 {
    if m == 0 {
        0
    } else {
        (m >> m.trailing_zeros()).count_ones()
    }
}

// --- sheets and mip chains -----------------------------------------------------------------

/// The widest grid axis a sheet is tiled into. Matches the viewer's own flipbook grid limit — a
/// sheet wider than that could not be read back by the transport that displays it.
const MAX_GRID_AXIS: u32 = 64;

/// How `frames` surfaces of one `kind` tile into a sheet, or `None` for a single surface (or more
/// surfaces than the widest grid holds).
pub(crate) fn sheet_layout(frames: u32, kind: SheetKind) -> Option<SheetLayout> {
    if frames < 2 {
        return None;
    }
    let (cols, rows) = layout_grid(frames)?;
    Some(SheetLayout {
        cols,
        rows,
        frames: frames.min(cols * rows),
        kind,
    })
}

/// The grid `frames` surfaces tile into: as near square as it gets, filled row-major.
///
/// A single strip would be simpler and much worse — six 4096 faces side by side is a 24576-wide
/// canvas, most of the way to a decode-guard rejection for a texture that tiles comfortably as
/// 3x2. Square-ish is also how a cubemap is conventionally drawn.
fn layout_grid(frames: u32) -> Option<(u32, u32)> {
    if frames == 0 || frames > MAX_GRID_AXIS * MAX_GRID_AXIS {
        return None;
    }
    let cols = (f64::from(frames).sqrt().ceil() as u32).clamp(1, MAX_GRID_AXIS);
    let rows = frames.div_ceil(cols).min(MAX_GRID_AXIS);
    Some((cols, rows))
}

/// The `TooLarge` a header's own arithmetic overflowing earns.
pub(crate) fn too_large(what: &str) -> DecodeError {
    DecodeError::TooLarge(format!("{what} sheet dimensions overflow"))
}

/// The decode guard, on the header's bare claim: one `width`x`height` surface, and — with
/// [`Surfaces::Sheet`] — the sheet the `declared` layout tiles it into.
///
/// It runs *first*, before anything else is read from the header. Order matters beyond
/// tidiness: a decode bomb has to come back as `TooLarge` — "we refused this" — and not as the
/// `Malformed` that a walk over its absurd (and therefore truncated) surface data would produce
/// first. And the sheet is guarded from the grid the *header* asks for, before the payload is
/// consulted at all: a guard that only fired once the data was known to be present would be no
/// guard, since the interesting case is a header claiming far more than the file could hold.
pub(crate) fn check_texture(
    cx: &Cx,
    out: PixelFormat,
    width: u32,
    height: u32,
    declared: Option<SheetLayout>,
    what: &str,
) -> Result<(), DecodeError> {
    let bpp = out.bytes_per_pixel();
    cx.check_dims(width as usize, height as usize, bpp, what)?;
    if cx.opts.surfaces == Surfaces::Sheet
        && let Some(l) = declared
    {
        cx.check_dims(
            width.checked_mul(l.cols).ok_or_else(|| too_large(what))? as usize,
            height.checked_mul(l.rows).ok_or_else(|| too_large(what))? as usize,
            bpp,
            &format!("{what} sheet"),
        )?;
    }
    Ok(())
}

/// One texture's surfaces, as [`assemble`] reads them.
pub(crate) struct Shape {
    /// One surface's size at level 0.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The levels stored, at least 1.
    pub(crate) levels: u32,
    /// How the surfaces the data actually backs tile, or `None` for a single surface.
    pub(crate) all: Option<SheetLayout>,
}

/// What [`assemble`] built: the canvas and its chain, ready for a [`DecodedImage`].
///
/// [`DecodedImage`]: crate::DecodedImage
pub(crate) struct Assembled {
    pub(crate) pixels: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) source_mips: Option<Vec<Vec<u8>>>,
    pub(crate) layout: Option<SheetLayout>,
}

/// Build the canvas a texture decodes to, from `cell(level, frame, w, h)`: frame `frame` of mip
/// `level`, a `w`x`h` surface decoded to `out`, or `None` when its bytes are missing.
///
/// With [`Surfaces::Sheet`] every surface is composited into one sheet described by a
/// [`SheetLayout`], so the viewer's flipbook can step through the faces or slices with the
/// transport it already has; with [`Surfaces::First`] it is the first surface alone. A single
/// surface is decoded straight into the canvas, with no tiling copy.
///
/// The file's own chain, levels 1.., comes back only when the setup keeps it (`stored_mips`), and
/// is not decoded otherwise. It is authored, often with a better filter than a box, and the whole
/// reason to look at a texture's mip rather than a computed one. A level whose bytes are missing
/// — or, for a sheet, whose surfaces no longer tile it exactly — ends the chain rather than
/// failing the decode: half an authored chain is still better than none, and the worker pads
/// whatever is short. A volume's levels are volumes, so its first slice alone keeps no chain.
///
/// `premultiplied` straightens the canvas and every level: colour stored already multiplied by
/// alpha, displayed directly, renders every semi-transparent area about twice too dark.
pub(crate) fn assemble(
    cx: &Cx,
    out: PixelFormat,
    shape: &Shape,
    what: &str,
    premultiplied: bool,
    mut cell: impl FnMut(u32, u32, u32, u32) -> Option<Vec<u8>>,
) -> Result<Assembled, DecodeError> {
    let (width, height) = (shape.width, shape.height);
    let layout = shape.all.filter(|_| cx.opts.surfaces == Surfaces::Sheet);
    let (cols, rows) = layout.map_or((1, 1), |l| (l.cols, l.rows));
    let (out_w, out_h) = (
        width.checked_mul(cols).ok_or_else(|| too_large(what))?,
        height.checked_mul(rows).ok_or_else(|| too_large(what))?,
    );

    let mut pixels = match layout {
        Some(l) => composite(out, width, height, 0, l, &mut cell),
        None => exact(out, width, height, cell(0, 0, width, height)),
    }
    .ok_or_else(|| DecodeError::Malformed(format!("{what} surface data is truncated")))?;

    let mut source_mips = Vec::new();
    let volume_slice =
        layout.is_none() && shape.all.is_some_and(|l| l.kind == SheetKind::VolumeSlices);
    let keep_mips = cx.opts.stored_mips && !volume_slice;
    for level in (1..shape.levels).filter(|_| keep_mips) {
        let next = match layout {
            None => {
                let (lw, lh) = level_dims(width, height, level);
                exact(out, lw, lh, cell(level, 0, lw, lh))
            }
            Some(l) => composite(out, width, height, level, l, &mut cell),
        };
        let Some(next) = next else { break };
        source_mips.push(next);
    }

    if premultiplied {
        straighten(&mut pixels, out);
        for level in &mut source_mips {
            straighten(level, out);
        }
    }

    Ok(Assembled {
        pixels,
        width: out_w,
        height: out_h,
        source_mips: (!source_mips.is_empty()).then_some(source_mips),
        layout,
    })
}

/// `cell` if it is exactly one `w`x`h` surface of `out`: a decoder that came back short or long
/// is a surface that isn't there.
fn exact(out: PixelFormat, w: u32, h: u32, cell: Option<Vec<u8>>) -> Option<Vec<u8>> {
    let len = (w as usize)
        .checked_mul(h as usize)?
        .checked_mul(out.bytes_per_pixel())?;
    cell.filter(|c| c.len() == len)
}

/// Decode every surface of mip `level` and tile them into one sheet, row-major.
///
/// `None` when a surface's bytes are missing, or — for a mip level — when the surfaces no longer
/// tile the sheet exactly. That second case is the honest one: past the point where
/// `cols * (w >> n)` stops equalling `(cols * w) >> n`, a tiled level is not a level *of* the
/// sheet it claims to belong to, and the chain would sample the wrong texels. For a
/// power-of-two face — which is nearly every real cubemap — it holds all the way down.
fn composite(
    out_format: PixelFormat,
    w: u32,
    h: u32,
    level: u32,
    layout: SheetLayout,
    cell: &mut impl FnMut(u32, u32, u32, u32) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let (fw, fh) = level_dims(w, h, level);
    let (sheet_w, sheet_h) = level_dims(
        w.checked_mul(layout.cols)?,
        h.checked_mul(layout.rows)?,
        level,
    );
    if fw.checked_mul(layout.cols)? != sheet_w || fh.checked_mul(layout.rows)? != sheet_h {
        return None;
    }

    let bpp = out_format.bytes_per_pixel();
    let sheet_row = (sheet_w as usize).checked_mul(bpp)?;
    let cell_row = (fw as usize).checked_mul(bpp)?;
    // Trailing cells of a partly filled grid stay transparent black.
    let mut out = vec![0u8; sheet_row.checked_mul(sheet_h as usize)?];

    for frame in 0..layout.frames {
        let cell = exact(out_format, fw, fh, cell(level, frame, fw, fh))?;
        let x0 = (frame % layout.cols) as usize * cell_row;
        let y0 = (frame / layout.cols) as usize * fh as usize;
        for y in 0..fh as usize {
            let dst = (y0 + y) * sheet_row + x0;
            out[dst..dst + cell_row].copy_from_slice(&cell[y * cell_row..y * cell_row + cell_row]);
        }
    }
    Some(out)
}

// --- decoding ------------------------------------------------------------------------------

/// Decode one surface into a fresh RGBA buffer. `src` is exactly that surface's bytes.
pub(crate) fn decode_surface(fmt: &Format, src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let bpp = fmt.out.bytes_per_pixel();
    let row = w as usize * bpp;
    let mut out = vec![0u8; row * h as usize];
    match fmt.codec {
        Codec::Block(b) => decode_blocks(b, src, w, h, &mut out, row, bpp),
        Codec::Plain(p) => decode_plain(p, src, fmt.out, &mut out),
    }
    out
}

/// Surfaces with at least this many texels are expanded on several threads. Below it the
/// hand-off costs more than the work — the same threshold the mip builder uses.
const PARALLEL_MIN_TEXELS: usize = 256 * 256;

/// The most threads one surface's blocks are expanded on, as before the budget took them over.
const MAX_BLOCK_THREADS: usize = 8;

/// Expand block-compressed `src` into `out`, one 4x4 block at a time.
///
/// The threads come from the budget (FD8), like every other pass: a band of block rows at a time,
/// on helpers that count as decoding. Spawned by hand, as they once were, they took cores the
/// budget had already lent elsewhere, and a panic on one of them ran the panic hook on a thread
/// [`in_decode`](crate::in_decode) knew nothing of — which 3D Review's hook answers by aborting.
fn decode_blocks(b: Block, src: &[u8], w: u32, h: u32, out: &mut [u8], row: usize, bpp: usize) {
    let bw = (w as usize).div_ceil(4);
    let bsz = b.block_bytes();
    let w = w as usize;

    // One unit of work is a block row: four output rows, fewer at the bottom edge.
    let block_row = |by: usize, dst: &mut [u8]| {
        let rows = dst.len() / row;
        for bx in 0..bw {
            let off = (by * bw + bx) * bsz;
            let Some(blk) = src.get(off..off + bsz) else {
                return;
            };
            let x0 = bx * 4;
            emit_block(b, blk, dst, row, x0 * bpp, (w - x0).min(4), rows);
        }
    };

    let band = 4 * row;
    if band == 0 {
        return;
    }
    let block_rows = (h as usize).div_ceil(4);
    let want = if w * h as usize >= PARALLEL_MIN_TEXELS {
        block_rows.min(MAX_BLOCK_THREADS)
    } else {
        1
    };
    crate::budget::run_chunks_mut(out, band, want, |offset, part| {
        for (j, dst) in part.chunks_mut(band).enumerate() {
            block_row(offset / band + j, dst);
        }
    });
}

/// Decode one 4x4 block and scatter its in-bounds texels into `dst`, a band of up to four output
/// rows. Each codec decodes into a scratch block of its own natural layout first, which is what
/// keeps the edge blocks of a non-multiple-of-4 surface from writing out of bounds.
fn emit_block(
    b: Block,
    blk: &[u8],
    dst: &mut [u8],
    row: usize,
    xoff: usize,
    cols: usize,
    rows: usize,
) {
    match b {
        Block::Bc1 | Block::Bc2 | Block::Bc3 | Block::Bc7 => {
            let f: fn(&[u8], &mut [u8], usize) = match b {
                Block::Bc1 => bcdec_rs::bc1,
                Block::Bc2 => bcdec_rs::bc2,
                Block::Bc3 => bcdec_rs::bc3,
                _ => bcdec_rs::bc7,
            };
            let mut s = [0u8; 4 * 4 * 4];
            f(blk, &mut s, 4 * 4);
            for y in 0..rows {
                dst[y * row + xoff..y * row + xoff + cols * 4]
                    .copy_from_slice(&s[y * 16..y * 16 + cols * 4]);
            }
        }
        Block::Bc4(signed) => {
            let mut s = [0u8; 4 * 4];
            bcdec_rs::bc4(blk, &mut s, 4, signed);
            for y in 0..rows {
                for x in 0..cols {
                    let v = snorm_to_unorm(s[y * 4 + x], signed);
                    dst[y * row + xoff + x * 4..][..4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        Block::Bc5(signed) => {
            let mut s = [0u8; 4 * 4 * 2];
            bcdec_rs::bc5(blk, &mut s, 4 * 2, signed);
            for y in 0..rows {
                for x in 0..cols {
                    let r = snorm_to_unorm(s[y * 8 + x * 2], signed);
                    let g = snorm_to_unorm(s[y * 8 + x * 2 + 1], signed);
                    dst[y * row + xoff + x * 4..][..4].copy_from_slice(&[r, g, 0, 255]);
                }
            }
        }
        Block::Bc6h(signed) => {
            let mut s = [0u16; 4 * 4 * 3];
            bcdec_rs::bc6h_half(blk, &mut s, 4 * 3, signed);
            for y in 0..rows {
                for x in 0..cols {
                    let p = y * 12 + x * 3;
                    let d = &mut dst[y * row + xoff + x * 8..][..8];
                    d[0..2].copy_from_slice(&s[p].to_ne_bytes());
                    d[2..4].copy_from_slice(&s[p + 1].to_ne_bytes());
                    d[4..6].copy_from_slice(&s[p + 2].to_ne_bytes());
                    d[6..8].copy_from_slice(&HALF_ONE.to_ne_bytes());
                }
            }
        }
    }
}

/// Re-centre a signed block channel for display. `bcdec` hands back the -127..127 range
/// reinterpreted as a byte, so a signed normal map would otherwise show as noise; this is the
/// mapping a sampler applies, with 0 landing on mid-grey.
fn snorm_to_unorm(v: u8, signed: bool) -> u8 {
    if !signed {
        return v;
    }
    let x = i32::from((v as i8).max(-127)) + 127; // 0..254
    ((x * 255 + 127) / 254) as u8
}

/// Expand an uncompressed surface into `out`, split across threads like the block codecs.
fn decode_plain(p: Plain, src: &[u8], out_format: PixelFormat, out: &mut [u8]) {
    let bpp = out_format.bytes_per_pixel();
    let stride = p.texel_bytes();
    crate::par_chunks_mut(out, bpp, |offset, part| {
        let src = src.get(offset / bpp * stride..).unwrap_or_default();
        match byte_lanes(p, out_format) {
            Some(lanes) => copy_byte_lanes(lanes, src, part),
            None => decode_plain_texels(p, src, out_format, part),
        }
    });
}

/// Where R, G, B and A sit in a 4-byte texel whose masks are each one whole byte (alpha may be
/// absent: `None`), for an 8-bit output — RGBA8, BGRA8 and their X variants, which is nearly every
/// uncompressed DDS. For those the general mask arithmetic reduces exactly to picking bytes.
fn byte_lanes(p: Plain, out_format: PixelFormat) -> Option<([usize; 3], Option<usize>)> {
    let Plain::Masked {
        bytes: 4,
        masks,
        luminance: false,
    } = p
    else {
        return None;
    };
    if out_format != PixelFormat::Rgba8Unorm {
        return None;
    }
    let lane = |m: u32| {
        let at = m.trailing_zeros();
        (at.is_multiple_of(8) && m >> at == 0xff).then_some(at as usize / 8)
    };
    let rgb = [lane(masks[0])?, lane(masks[1])?, lane(masks[2])?];
    let alpha = match masks[3] {
        0 => None,
        m => Some(lane(m)?),
    };
    Some((rgb, alpha))
}

fn copy_byte_lanes(([r, g, b], alpha): ([usize; 3], Option<usize>), src: &[u8], out: &mut [u8]) {
    let texels = src.as_chunks::<4>().0;
    let out = out.as_chunks_mut::<4>().0;
    match alpha {
        Some(a) => {
            for (d, s) in out.iter_mut().zip(texels) {
                *d = [s[r], s[g], s[b], s[a]];
            }
        }
        None => {
            for (d, s) in out.iter_mut().zip(texels) {
                *d = [s[r], s[g], s[b], 255];
            }
        }
    }
}

/// [`decode_plain`]'s general case: one texel at a time, any layout.
fn decode_plain_texels(p: Plain, src: &[u8], out_format: PixelFormat, out: &mut [u8]) {
    let bpp = out_format.bytes_per_pixel();
    let texels = out.len() / bpp;
    let stride = p.texel_bytes();
    for i in 0..texels {
        let Some(unit) = src.get(i * stride..i * stride + stride) else {
            return;
        };
        let d = &mut out[i * bpp..i * bpp + bpp];
        match p {
            Plain::Masked {
                masks, luminance, ..
            } => {
                let word = read_le(unit);
                let to = if out_format == PixelFormat::Rgba16Unorm {
                    0xffff
                } else {
                    0xff
                };
                let r = channel(word, masks[0], to);
                let (g, b) = if luminance {
                    (r, r)
                } else {
                    (channel(word, masks[1], to), channel(word, masks[2], to))
                };
                let a = if masks[3] == 0 {
                    to
                } else {
                    channel(word, masks[3], to)
                };
                if to == 0xffff {
                    store_u16(d, [r as u16, g as u16, b as u16, a as u16]);
                } else {
                    d.copy_from_slice(&[r as u8, g as u8, b as u8, a as u8]);
                }
            }
            Plain::U16(n) => {
                store_u16(
                    d,
                    spread(n, u16::MAX, 0, |c| {
                        u16::from_le_bytes([unit[c * 2], unit[c * 2 + 1]])
                    }),
                );
            }
            Plain::F16(n) => {
                store_u16(
                    d,
                    spread(n, HALF_ONE, 0, |c| {
                        u16::from_le_bytes([unit[c * 2], unit[c * 2 + 1]])
                    }),
                );
            }
            Plain::F32(n) => {
                store_f32(
                    d,
                    spread(n, 1.0, 0.0, |c| {
                        f32::from_le_bytes([
                            unit[c * 4],
                            unit[c * 4 + 1],
                            unit[c * 4 + 2],
                            unit[c * 4 + 3],
                        ])
                    }),
                );
            }
            Plain::R11G11B10 => {
                let w = read_le(unit);
                store_f32(
                    d,
                    [
                        packed_float_to_f32(w & 0x7ff, 6),
                        packed_float_to_f32((w >> 11) & 0x7ff, 6),
                        packed_float_to_f32((w >> 22) & 0x3ff, 5),
                        1.0,
                    ],
                );
            }
            Plain::R9G9B9E5 => {
                let w = read_le(unit);
                // A 5-bit exponent biased by 15, shared by three 9-bit fractional mantissas.
                let scale = 2f32.powi(((w >> 27) & 0x1f) as i32 - 15 - 9);
                store_f32(
                    d,
                    [
                        (w & 0x1ff) as f32 * scale,
                        ((w >> 9) & 0x1ff) as f32 * scale,
                        ((w >> 18) & 0x1ff) as f32 * scale,
                        1.0,
                    ],
                );
            }
        }
    }
}

/// Widen `n` source channels to RGBA, replicating a single channel across the colour lanes (a
/// grey source) and zeroing the blue lane of a two-channel one.
fn spread<T: Copy>(n: u8, opaque: T, zero: T, v: impl Fn(usize) -> T) -> [T; 4] {
    match n {
        1 => [v(0), v(0), v(0), opaque],
        2 => [v(0), v(1), zero, opaque],
        3 => [v(0), v(1), v(2), opaque],
        _ => [v(0), v(1), v(2), v(3)],
    }
}

/// A 1-, 2-, 3- or 4-byte little-endian unit as a `u32`.
fn read_le(unit: &[u8]) -> u32 {
    let mut w = 0u32;
    for (i, b) in unit.iter().take(4).enumerate() {
        w |= u32::from(*b) << (8 * i);
    }
    w
}

/// Pull one masked channel out of `word` and rescale it to `0..=to`. Bit-exact for a mask as
/// wide as the target (8 or 16 bits) and correctly rounded for every other width.
fn channel(word: u32, mask: u32, to: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    let v = u64::from((word & mask) >> mask.trailing_zeros());
    let max = ((1u64 << mask_width(mask)) - 1).max(1);
    ((v * u64::from(to) + max / 2) / max) as u32
}

fn store_u16(d: &mut [u8], v: [u16; 4]) {
    for (i, x) in v.iter().enumerate() {
        d[i * 2..i * 2 + 2].copy_from_slice(&x.to_ne_bytes());
    }
}

fn store_f32(d: &mut [u8], v: [f32; 4]) {
    for (i, x) in v.iter().enumerate() {
        d[i * 4..i * 4 + 4].copy_from_slice(&x.to_ne_bytes());
    }
}

/// One unsigned packed float — the 11- and 10-bit lanes of `R11G11B10_Float` — with
/// `mantissa_bits` of mantissa and a 5-bit exponent biased by 15. No sign bit.
fn packed_float_to_f32(v: u32, mantissa_bits: u32) -> f32 {
    let max = (1u32 << mantissa_bits) - 1;
    let mantissa = v & max;
    let exponent = v >> mantissa_bits;
    let frac = mantissa as f32 / (max + 1) as f32;
    if exponent == 0 {
        // Denormal: no implicit leading 1.
        frac * 2f32.powi(-14)
    } else if exponent == 0x1f {
        if mantissa == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + frac) * 2f32.powi(exponent as i32 - 15)
    }
}

/// Divide the colour channels back out of a premultiplied buffer.
fn straighten(pixels: &mut [u8], format: PixelFormat) {
    match format {
        PixelFormat::Rgba8Unorm => {
            for px in pixels.as_chunks_mut::<4>().0 {
                let a = u32::from(px[3]);
                if a == 0 || a == 255 {
                    continue;
                }
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        PixelFormat::Rgba16Unorm => {
            for px in pixels.as_chunks_mut::<8>().0 {
                let a = u64::from(u16::from_ne_bytes([px[6], px[7]]));
                if a == 0 || a == 65535 {
                    continue;
                }
                for c in 0..3 {
                    let v = u64::from(u16::from_ne_bytes([px[c * 2], px[c * 2 + 1]]));
                    let s = ((v * 65535 + a / 2) / a).min(65535) as u16;
                    px[c * 2..c * 2 + 2].copy_from_slice(&s.to_ne_bytes());
                }
            }
        }
        PixelFormat::Rgba32Float => {
            for px in pixels.as_chunks_mut::<16>().0 {
                let a = f32::from_ne_bytes([px[12], px[13], px[14], px[15]]);
                if a <= 0.0 || a == 1.0 {
                    continue;
                }
                for c in 0..3 {
                    let b: [u8; 4] = px[c * 4..c * 4 + 4].try_into().unwrap();
                    let s = f32::from_ne_bytes(b) / a;
                    px[c * 4..c * 4 + 4].copy_from_slice(&s.to_ne_bytes());
                }
            }
        }
        // The only half-float source is BC6H, which has no alpha channel to have multiplied by.
        PixelFormat::Rgba16Float => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `decode_plain` — threaded, with the byte-lane shortcut for whole-byte masks — against the
    /// per-texel decoder it replaced, over the layouts that take the shortcut and ones that must
    /// not. The surface is large enough to be split across threads, and the last case's source
    /// stops short, which has to leave the same zeroed tail either way.
    #[test]
    fn plain_surfaces_match_the_per_texel_decoder() {
        let texels = 600 * 700usize;
        let src: Vec<u8> = (0..texels * 4)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 9) as u8)
            .collect();
        let masked = |bytes, masks| Plain::Masked {
            bytes,
            masks,
            luminance: false,
        };
        let cases = [
            masked(4, [0xff_0000, 0xff00, 0xff, 0xff00_0000]), // BGRA8
            masked(4, [0xff, 0xff00, 0xff_0000, 0xff00_0000]), // RGBA8
            masked(4, [0xff_0000, 0xff00, 0xff, 0]),           // BGRX8
            masked(2, [0xf800, 0x07e0, 0x001f, 0]),            // B5G6R5: not whole bytes
            masked(4, [0x3ff, 0xffc00, 0x3ff0_0000, 0]),       // 10-bit lanes
        ];
        for (i, p) in cases.into_iter().enumerate() {
            let src = &src[..texels * p.texel_bytes()];
            let mut expected = vec![0u8; texels * 4];
            decode_plain_texels(p, src, PixelFormat::Rgba8Unorm, &mut expected);
            let mut out = vec![0u8; texels * 4];
            decode_plain(p, src, PixelFormat::Rgba8Unorm, &mut out);
            assert!(out == expected, "case {i}");
        }
        let p = masked(4, [0xff_0000, 0xff00, 0xff, 0xff00_0000]);
        let short = &src[..texels * 4 - 1000];
        let mut expected = vec![0u8; texels * 4];
        decode_plain_texels(p, short, PixelFormat::Rgba8Unorm, &mut expected);
        let mut out = vec![0u8; texels * 4];
        decode_plain(p, short, PixelFormat::Rgba8Unorm, &mut out);
        assert!(out == expected, "short source");
    }

    /// The block expander, split across the budget's threads in bands of block rows, against the
    /// same blocks expanded one at a time in order. The surface is past the threading threshold
    /// and neither axis is a multiple of 4, so the bands and the edge blocks both get cut.
    #[test]
    fn threaded_blocks_match_one_block_at_a_time() {
        let (w, h) = (603u32, 701u32);
        let (bw, bh) = ((w as usize).div_ceil(4), (h as usize).div_ceil(4));
        for b in [Block::Bc1, Block::Bc7, Block::Bc4(true), Block::Bc6h(false)] {
            let fmt = Format::block(
                b,
                if matches!(b, Block::Bc6h(_)) {
                    PixelFormat::Rgba16Float
                } else {
                    PixelFormat::Rgba8Unorm
                },
                8,
                4,
                "test",
            );
            let src: Vec<u8> = (0..fmt.level_bytes(w, h, 1).unwrap())
                .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
                .collect();
            let bpp = fmt.out.bytes_per_pixel();
            let row = w as usize * bpp;
            let mut expected = vec![0u8; row * h as usize];
            for by in 0..bh {
                let rows = (h as usize - by * 4).min(4);
                let band = &mut expected[by * 4 * row..(by * 4 + rows) * row];
                for bx in 0..bw {
                    let blk = &src[(by * bw + bx) * b.block_bytes()..][..b.block_bytes()];
                    let cols = (w as usize - bx * 4).min(4);
                    emit_block(b, blk, band, row, bx * 4 * bpp, cols, rows);
                }
            }
            assert!(decode_surface(&fmt, &src, w, h) == expected, "{b:?}");
        }
    }
}
