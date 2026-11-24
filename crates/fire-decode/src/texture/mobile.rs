//! The block codecs KTX and KTX2 carry and DDS doesn't: ETC1, ETC2 and EAC (the OpenGL ES 3.0
//! and Vulkan baseline), ASTC (LDR, every 2D footprint) and PVRTC1, decoded by
//! `texture2ddecoder`, a pure-Rust port of AssetStudio's decoders.
//!
//! Every one of them decodes to `Rgba8Unorm`. EAC's 11-bit channels are cut to 8 bits on the way
//! (the decoder rounds them so); the status bar still reports the 11 the file stores. EAC R is
//! one channel, shown grey like BC4, and EAC RG is the normal-map pair, shown `(r, g, 0, 1)` like
//! BC5. The signed spellings are re-centred by the decoder itself, zero landing on mid-grey.

use texture2ddecoder as t2d;

use super::{MAX_BLOCK_THREADS, PARALLEL_MIN_TEXELS};

/// A mobile block codec. The `bool`s are "the channels are signed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mobile {
    Etc1,
    Etc2Rgb,
    /// ETC2 with a punch-through (1-bit) alpha.
    Etc2Rgba1,
    /// ETC2 colour plus an EAC alpha block.
    Etc2Rgba8,
    EacR(bool),
    EacRg(bool),
    /// ASTC LDR, with its block footprint: one of the 14 the format defines, 4x4 to 12x12.
    Astc(u8, u8),
    /// PVRTC1, 2 or 4 bits per texel. Its blocks borrow colour from their neighbours, wrapping
    /// at the edges, so it decodes a whole surface at a time and only at power-of-two sizes.
    Pvrtc1 {
        two_bpp: bool,
    },
}

impl Mobile {
    /// The block's width and height in texels, and its size in bytes.
    fn block(self) -> (usize, usize, usize) {
        match self {
            Mobile::Etc1 | Mobile::Etc2Rgb | Mobile::Etc2Rgba1 | Mobile::EacR(_) => (4, 4, 8),
            Mobile::Etc2Rgba8 | Mobile::EacRg(_) => (4, 4, 16),
            Mobile::Astc(w, h) => (w as usize, h as usize, 16),
            Mobile::Pvrtc1 { two_bpp: true } => (8, 4, 8),
            Mobile::Pvrtc1 { two_bpp: false } => (4, 4, 8),
        }
    }

    /// The size a PVRTC1 surface is stored and decoded at: never fewer than 2x2 blocks, however
    /// small the level. `None` for the other codecs, which store exactly the blocks they cover.
    fn pvrtc_padded(self, w: usize, h: usize) -> Option<(usize, usize)> {
        if !matches!(self, Mobile::Pvrtc1 { .. }) {
            return None;
        }
        let (bw, bh, _) = self.block();
        Some((w.max(2 * bw), h.max(2 * bh)))
    }
}

/// Bytes one `w`x`h`x`d` surface of `m` occupies in the file, or `None` if that overflows.
pub(crate) fn level_bytes(m: Mobile, w: u32, h: u32, d: u32) -> Option<usize> {
    let (bw, bh, bytes) = m.block();
    let (mut w, mut h) = (w as usize, h as usize);
    if let Some((pw, ph)) = m.pvrtc_padded(w, h) {
        (w, h) = (pw, ph);
    }
    w.div_ceil(bw)
        .checked_mul(h.div_ceil(bh))?
        .checked_mul(bytes)?
        .checked_mul(d as usize)
}

/// Decode one `w`x`h` surface of `m` from `src` into `out`, RGBA8. Texels whose blocks are
/// missing from `src` stay zero.
pub(crate) fn decode(m: Mobile, src: &[u8], w: u32, h: u32, out: &mut [u8]) {
    if matches!(m, Mobile::Pvrtc1 { .. }) {
        decode_pvrtc(m, src, w as usize, h as usize, out);
        return;
    }
    let (bw, bh, bsz) = m.block();
    let w = w as usize;
    let row = w * 4;
    let band = bh * row;
    if band == 0 {
        return;
    }
    let blocks_x = w.div_ceil(bw);
    let grey = matches!(m, Mobile::EacR(_));

    // One unit of work is a row of blocks: `bh` output rows, fewer at the bottom edge.
    let block_row = |by: usize, dst: &mut [u8]| {
        let rows = dst.len() / row;
        let mut s = [0u32; 12 * 12];
        for bx in 0..blocks_x {
            let off = (by * blocks_x + bx) * bsz;
            let Some(blk) = src.get(off..off + bsz) else {
                return;
            };
            decode_block(m, blk, bw, bh, &mut s);
            let (x0, cols) = (bx * bw, (w - bx * bw).min(bw));
            for y in 0..rows {
                let d = &mut dst[y * row + x0 * 4..][..cols * 4];
                for (x, px) in d.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    *px = rgba(s[y * bw + x], grey);
                }
            }
        }
    };

    let h = h as usize;
    let block_rows = h.div_ceil(bh);
    let want = if w * h >= PARALLEL_MIN_TEXELS {
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

/// One block into `s`, row-major at the block's own width.
fn decode_block(m: Mobile, blk: &[u8], bw: usize, bh: usize, s: &mut [u32; 144]) {
    match m {
        Mobile::Etc1 => t2d::decode_etc1_block(blk, s),
        Mobile::Etc2Rgb => t2d::decode_etc2_rgb_block(blk, s),
        Mobile::Etc2Rgba1 => t2d::decode_etc2_rgba1_block(blk, s),
        Mobile::Etc2Rgba8 => t2d::decode_etc2_rgba8_block(blk, s),
        Mobile::EacR(false) => t2d::decode_eacr_block(blk, s),
        Mobile::EacR(true) => t2d::decode_eacr_signed_block(blk, s),
        Mobile::EacRg(false) => t2d::decode_eacrg_block(blk, s),
        Mobile::EacRg(true) => t2d::decode_eacrg_signed_block(blk, s),
        Mobile::Astc(..) => t2d::decode_astc_block(blk, bw, bh, s),
        Mobile::Pvrtc1 { .. } => {}
    }
}

/// The decoder's packed texel — `u32::from_le_bytes([b, g, r, a])`, whatever the host's byte
/// order — as RGBA bytes, red replicated to grey for a one-channel source.
fn rgba(c: u32, grey: bool) -> [u8; 4] {
    let [b, g, r, a] = c.to_le_bytes();
    if grey { [r, r, r, a] } else { [r, g, b, a] }
}

/// PVRTC1: the whole surface at its padded size, then the `w`x`h` corner of it. The decoder
/// wants a power-of-two number of blocks on each side; the container refuses any other size
/// before it gets here, and a level smaller than 2x2 blocks is stored padded up to that.
fn decode_pvrtc(m: Mobile, src: &[u8], w: usize, h: usize, out: &mut [u8]) {
    let Mobile::Pvrtc1 { two_bpp } = m else {
        return;
    };
    let Some((pw, ph)) = m.pvrtc_padded(w, h) else {
        return;
    };
    // The decoder's own length check is short by half for 4 bpp, so it is made here.
    let Some(need) = level_bytes(m, w as u32, h as u32, 1) else {
        return;
    };
    let Some(src) = src.get(..need) else {
        return;
    };
    let mut img = vec![0u32; pw * ph];
    if t2d::decode_pvrtc(src, pw, ph, &mut img, two_bpp).is_err() {
        return;
    }
    for (y, dst) in out.chunks_exact_mut(w * 4).take(h).enumerate() {
        for (x, px) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *px = rgba(img[y * pw + x], false);
        }
    }
}
