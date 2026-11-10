//! Mip chains, built on the CPU (FD11): the `mips` feature.
//!
//! fire and 3D Review both upload a texture with its whole chain, because sokol_gfx has no
//! `GenerateMips` and an immutable image takes every level as data at creation. This is the one
//! builder both use, moved here from fire's `render/mips.rs`, with 3D Review's raw-average mode
//! and level cap added:
//! - **Each level is a 2×2 box filter of the level above**, what a hardware `GenerateMips` does.
//!   Odd edges clamp, so an odd axis repeats its last row or column rather than reading past it.
//! - **An 8-bit buffer's colour is averaged in linear light** when [`MipOptions::srgb`] is on, as
//!   the hardware does for an `*_SRGB` format, through two lookup tables. Off, the stored bytes
//!   are averaged, as for a `UNORM` one: 3D Review's data textures and its Tex viewport.
//! - **Alpha, and the 16-bit and float formats, are averaged as stored.** Integer samples round
//!   to nearest exactly, ties up.
//! - **A file's own chain is kept** ([`complete`]): a DDS's authored levels are adopted and only
//!   the tail it stopped short of is computed.
//! - **The big levels are split across threads** from the process-wide budget (FD8), so a chain
//!   built while decodes run takes only what they leave free.
//!
//! An 8-bit chain is byte-identical to 3D Review's `rhi/mips.rs`, which that repository keeps for
//! its build-time UV-checker bake and holds to this one with a test.

use std::sync::OnceLock;

use exr::prelude::f16;

use crate::{PixelFormat, budget, level_dims};

/// How [`chain`], [`complete`] and [`fit`] build levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MipOptions {
    /// Whether an 8-bit buffer's RGB is sRGB-encoded colour, averaged in linear light. Off, its
    /// bytes are averaged as stored. The 16-bit and float formats have no sRGB form and are
    /// always averaged as stored.
    pub srgb: bool,
    /// The most levels a chain has, level 0 included, or 0 for as many as the dimensions allow.
    /// 3D Review passes sokol's `SG_MAX_MIPMAPS`, so the crate needn't know it.
    pub max_levels: u32,
    /// The most threads a level is split across, the caller's included, or 0 for what the
    /// process-wide budget has free (FD8). fire passes 1 for a PSD layer it prefetches, so the
    /// chain never competes with the image the user is waiting for.
    pub max_threads: usize,
}

impl MipOptions {
    /// Options with `srgb` set, no level cap beyond the dimensions, and threads from the budget.
    pub fn new(srgb: bool) -> MipOptions {
        MipOptions {
            srgb,
            max_levels: 0,
            max_threads: 0,
        }
    }

    /// The levels a `width`×`height` chain has under these options, level 0 included.
    fn levels(&self, width: u32, height: u32) -> u32 {
        let full = level_count(width, height);
        match self.max_levels {
            0 => full,
            cap => full.min(cap),
        }
    }

    /// Run `f` under this thread cap. Without one, `f` inherits the calling thread's.
    fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        match self.max_threads {
            0 => f(),
            n => budget::with_cap(n, f),
        }
    }
}

/// The levels in a full chain for a `width`×`height` image, level 0 included: one more than the
/// halvings it takes the longer side to reach 1.
pub fn level_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

/// Levels `1..` of the chain for `level0`, a `width`×`height` image in `format`: one buffer per
/// level, tightly packed in `format`, smallest last. Empty for a 1×1 image, and for a `level0`
/// shorter than its dimensions declare, rather than a read past its end.
pub fn chain(
    level0: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    opts: &MipOptions,
) -> Vec<Vec<u8>> {
    if !holds(level0, width, height, format) {
        return Vec::new();
    }
    opts.run(|| {
        let levels = opts.levels(width, height);
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(levels.saturating_sub(1) as usize);
        for level in 1..levels {
            let src = out.last().map_or(level0, Vec::as_slice);
            out.push(halve(src, width, height, level, format, opts.srgb));
        }
        out
    })
}

/// A file's own chain (`stored`, levels `1..`, as [`DecodedImage::source_mips`] carries it),
/// completed into the full one: what [`chain`] returns, with the file's levels in place of the
/// computed ones.
///
/// A file may stop short (a DDS is free to end at 4×4), claim more levels than its dimensions
/// allow, or be wrong. Its levels are kept while each one's length matches its dimensions; the
/// first that does not ends the adopted run, since past it the file's own offsets can't be
/// trusted. The tail is then filtered down from the last level kept, not from level 0, so the
/// authored levels stay upstream of everything computed: authored mips are the reason to look at
/// a texture's mips at all. Empty for a `level0` shorter than its dimensions.
///
/// [`DecodedImage::source_mips`]: crate::DecodedImage::source_mips
pub fn complete(
    stored: Vec<Vec<u8>>,
    level0: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    opts: &MipOptions,
) -> Vec<Vec<u8>> {
    if !holds(level0, width, height, format) {
        return Vec::new();
    }
    let adopted = stored_levels(&stored, width, height, format, opts);
    opts.run(|| {
        let levels = opts.levels(width, height) as usize;
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(levels.saturating_sub(1));
        out.extend(stored.into_iter().take(adopted));
        while out.len() + 1 < levels {
            let src = out.last().map_or(level0, Vec::as_slice);
            let level = out.len() as u32 + 1;
            out.push(halve(src, width, height, level, format, opts.srgb));
        }
        out
    })
}

/// How many of a file's `stored` levels (`1..`) [`complete`] keeps: the leading run whose lengths
/// match their dimensions, within the chain the options allow. These are the levels the file
/// authored, as opposed to the tail [`complete`] computes.
pub fn stored_levels(
    stored: &[Vec<u8>],
    width: u32,
    height: u32,
    format: PixelFormat,
    opts: &MipOptions,
) -> usize {
    let bpp = format.bytes_per_pixel();
    let max = (opts.levels(width, height) as usize).saturating_sub(1);
    stored
        .iter()
        .take(max)
        .enumerate()
        .take_while(|(i, level)| {
            let (w, h) = level_dims(width, height, *i as u32 + 1);
            level.len() == w as usize * h as usize * bpp
        })
        .count()
}

/// `level0` halved, by the chain's own filter, until neither side exceeds `max_side`: the mip a
/// GPU samples for the image drawn that small, as `(pixels, width, height)`. For a caller that
/// never draws it larger, and so needn't upload or build the levels above. `None` when it already
/// fits, or when `level0` is shorter than its dimensions declare.
pub fn fit(
    level0: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    max_side: u32,
    opts: &MipOptions,
) -> Option<(Vec<u8>, u32, u32)> {
    let max_side = max_side.max(1);
    if width.max(height) <= max_side || !holds(level0, width, height, format) {
        return None;
    }
    opts.run(|| {
        let mut out: Option<Vec<u8>> = None;
        let mut level = 0;
        loop {
            let (w, h) = level_dims(width, height, level);
            if w.max(h) <= max_side {
                return out.map(|pixels| (pixels, w, h));
            }
            level += 1;
            let src = out.as_deref().unwrap_or(level0);
            out = Some(halve(src, width, height, level, format, opts.srgb));
        }
    })
}

/// Whether `pixels` holds a whole `width`×`height` image in `format`, and has a texel at all.
fn holds(pixels: &[u8], width: u32, height: u32, format: PixelFormat) -> bool {
    let needed = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(format.bytes_per_pixel()));
    width > 0 && height > 0 && needed.is_some_and(|n| pixels.len() >= n)
}

/// Level `level` of a `width`×`height` chain, from `src`, level `level - 1`.
fn halve(
    src: &[u8],
    width: u32,
    height: u32,
    level: u32,
    format: PixelFormat,
    srgb: bool,
) -> Vec<u8> {
    let (sw, sh) = level_dims(width, height, level - 1);
    let (dw, dh) = level_dims(width, height, level);
    let dims = (sw as usize, sh as usize, dw as usize, dh as usize);
    match format {
        PixelFormat::Rgba8Unorm if srgb => {
            let t = srgb_tables();
            reduce::<4>(src, dims, |[a, b, c, d], out| {
                for ch in 0..3 {
                    let sum = t.to_linear[a[ch] as usize]
                        + t.to_linear[b[ch] as usize]
                        + t.to_linear[c[ch] as usize]
                        + t.to_linear[d[ch] as usize];
                    out[ch] = t.encode(sum * 0.25);
                }
                out[3] = mean_u8(a[3], b[3], c[3], d[3]);
            })
        }
        PixelFormat::Rgba8Unorm => reduce::<4>(src, dims, |[a, b, c, d], out| {
            for ch in 0..4 {
                out[ch] = mean_u8(a[ch], b[ch], c[ch], d[ch]);
            }
        }),
        PixelFormat::Rgba16Unorm => reduce::<8>(src, dims, |[a, b, c, d], out| {
            for ch in 0..4 {
                let at = |t: &[u8; 8]| u32::from(u16::from_ne_bytes([t[2 * ch], t[2 * ch + 1]]));
                let mean = ((at(a) + at(b) + at(c) + at(d) + 2) / 4) as u16;
                out[2 * ch..2 * ch + 2].copy_from_slice(&mean.to_ne_bytes());
            }
        }),
        PixelFormat::Rgba16Float => {
            let t = f16_table();
            reduce::<8>(src, dims, |[a, b, c, d], out| {
                for ch in 0..4 {
                    let at =
                        |p: &[u8; 8]| t[u16::from_ne_bytes([p[2 * ch], p[2 * ch + 1]]) as usize];
                    let mean = (at(a) + at(b) + at(c) + at(d)) * 0.25;
                    out[2 * ch..2 * ch + 2]
                        .copy_from_slice(&f16::from_f32(mean).to_bits().to_ne_bytes());
                }
            })
        }
        PixelFormat::Rgba32Float => reduce::<16>(src, dims, |[a, b, c, d], out| {
            for ch in 0..4 {
                let at = |p: &[u8; 16]| {
                    f32::from_ne_bytes([p[4 * ch], p[4 * ch + 1], p[4 * ch + 2], p[4 * ch + 3]])
                };
                let mean = (at(a) + at(b) + at(c) + at(d)) * 0.25;
                out[4 * ch..4 * ch + 4].copy_from_slice(&mean.to_ne_bytes());
            }
        }),
    }
}

/// Levels with at least this many texels are split across threads; below it the hand-off costs
/// more than it saves. fire's threshold, kept.
const PARALLEL_MIN_TEXELS: usize = 256 * 256;

/// The most threads a level takes: fire's cap, kept. Each output texel reads four, so the work is
/// bound by memory as much as by the filter.
const PASS_THREADS: usize = 8;

/// One 2×2 box reduction of a `sw`×`sh` level (`E` bytes a texel) into `dw`×`dh`, with `texel`
/// averaging each output texel's four taps. `texel` is generic rather than a `fn` pointer so each
/// format gets its own copy of the loop with its arithmetic inlined: through a pointer, every
/// texel paid an indirect call, which was most of the cost in fire's measurements.
fn reduce<const E: usize>(
    src: &[u8],
    (sw, sh, dw, dh): (usize, usize, usize, usize),
    texel: impl Fn([&[u8; E]; 4], &mut [u8; E]) + Sync,
) -> Vec<u8> {
    let src_row = sw * E;
    let dst_row = dw * E;
    let mut out = vec![0u8; dst_row * dh];
    let rows = |offset: usize, part: &mut [u8]| {
        let first = offset / dst_row;
        for (i, drow) in part.chunks_exact_mut(dst_row).enumerate() {
            let y = first + i;
            let y0 = (2 * y).min(sh - 1);
            let y1 = (2 * y + 1).min(sh - 1);
            let r0 = src[y0 * src_row..(y0 + 1) * src_row].as_chunks::<E>().0;
            let r1 = src[y1 * src_row..(y1 + 1) * src_row].as_chunks::<E>().0;
            for (x, out) in drow.as_chunks_mut::<E>().0.iter_mut().enumerate() {
                let x0 = (2 * x).min(sw - 1);
                let x1 = (2 * x + 1).min(sw - 1);
                texel([&r0[x0], &r0[x1], &r1[x0], &r1[x1]], out);
            }
        }
    };
    let want = if dw * dh >= PARALLEL_MIN_TEXELS {
        PASS_THREADS
    } else {
        1
    };
    budget::run_chunks_mut(&mut out, dst_row, want, rows);
    out
}

/// The mean of four bytes, rounded to nearest, ties up.
fn mean_u8(a: u8, b: u8, c: u8, d: u8) -> u8 {
    ((u32::from(a) + u32::from(b) + u32::from(c) + u32::from(d) + 2) / 4) as u8
}

/// Steps in the linear → sRGB table. Its index is the linear value quantized to this many steps,
/// which resolves the curve's steep dark end to under one output code: the rounding error is that
/// of the 8-bit result itself, for 4352 `powf`s in place of one per channel per texel.
const ENCODE_STEPS: usize = 4096;

/// The two sRGB tables, built once per process.
struct SrgbTables {
    to_linear: [f32; 256],
    to_srgb: [u8; ENCODE_STEPS],
}

impl SrgbTables {
    /// A linear value in 0..1 encoded as an sRGB byte, through the table.
    fn encode(&self, linear: f32) -> u8 {
        self.to_srgb[round_half_up(linear * (ENCODE_STEPS - 1) as f32).min(ENCODE_STEPS - 1)]
    }
}

/// `x` (not negative) rounded to the nearest integer, ties up: `f32::round` for the values the
/// filter makes, without its library call on x86. `x - trunc(x)` is exact, so the tie test is too.
fn round_half_up(x: f32) -> usize {
    let whole = x as usize;
    whole + usize::from(x - whole as f32 >= 0.5)
}

fn srgb_tables() -> &'static SrgbTables {
    static TABLES: OnceLock<SrgbTables> = OnceLock::new();
    TABLES.get_or_init(|| SrgbTables {
        to_linear: std::array::from_fn(|code| {
            let c = code as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }),
        to_srgb: std::array::from_fn(|step| {
            let c = step as f32 / (ENCODE_STEPS - 1) as f32;
            let s = if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            };
            (s * 255.0).round().clamp(0.0, 255.0) as u8
        }),
    })
}

/// Every half float, widened. The filter reads four half texels for each one it writes, and a
/// lookup is several times cheaper than the conversion.
fn f16_table() -> &'static [f32; 65536] {
    static TABLE: OnceLock<Box<[f32; 65536]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([0f32; 65536]);
        for (bits, v) in table.iter_mut().enumerate() {
            *v = f16::from_bits(bits as u16).to_f32();
        }
        table
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid `w`×`h` RGBA8 buffer of `v`.
    fn solid(w: u32, h: u32, v: u8) -> Vec<u8> {
        vec![v; w as usize * h as usize * 4]
    }

    /// A reproducible buffer of `n` bytes that changes in every texel.
    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    const SRGB: MipOptions = MipOptions {
        srgb: true,
        max_levels: 0,
        max_threads: 0,
    };
    const RAW: MipOptions = MipOptions {
        srgb: false,
        max_levels: 0,
        max_threads: 0,
    };

    #[test]
    fn a_chain_has_one_more_level_than_halvings() {
        assert_eq!(level_count(1, 1), 1);
        assert_eq!(level_count(2, 1), 2);
        assert_eq!(level_count(256, 128), 9);
        assert_eq!(level_count(1000, 3), 10);
        assert_eq!(level_count(1024, 4), 11);
    }

    /// Each level floor-halves and stops at 1, and is tightly packed, so a short level shows up as
    /// a wrong length rather than as garbled rows.
    #[test]
    fn each_level_halves_down_to_one_texel() {
        let chain = chain(&solid(5, 3, 0), 5, 3, PixelFormat::Rgba8Unorm, &SRGB);
        let sizes: Vec<usize> = chain.iter().map(Vec::len).collect();
        assert_eq!(sizes, [2 * 4, 4], "5×3 → 2×1 → 1×1");
        let chain = super::chain(&solid(8, 4, 0), 8, 4, PixelFormat::Rgba8Unorm, &RAW);
        let sizes: Vec<usize> = chain.iter().map(Vec::len).collect();
        assert_eq!(sizes, [4 * 2 * 4, 2 * 4, 4]);
    }

    /// A buffer shorter than its dimensions, or an image with no texels, gets no chain rather than
    /// a read past the end.
    #[test]
    fn a_short_buffer_gets_no_chain() {
        let px = solid(5, 3, 0);
        assert!(chain(&px[..8], 5, 3, PixelFormat::Rgba8Unorm, &SRGB).is_empty());
        assert!(chain(&[], 0, 7, PixelFormat::Rgba8Unorm, &SRGB).is_empty());
        assert!(chain(&px, 5, 3, PixelFormat::Rgba16Unorm, &SRGB).is_empty());
    }

    /// `max_levels` caps the chain, level 0 included: sokol holds 16.
    #[test]
    fn max_levels_caps_the_chain() {
        let mut opts = MipOptions::new(true);
        opts.max_levels = 3;
        assert_eq!(
            chain(&solid(64, 64, 9), 64, 64, PixelFormat::Rgba8Unorm, &opts).len(),
            2
        );
        opts.max_levels = 99;
        assert_eq!(
            chain(&solid(64, 64, 9), 64, 64, PixelFormat::Rgba8Unorm, &opts).len(),
            6
        );
    }

    /// Black and white average to linear 0.5, which is sRGB 188, not the byte average 128 that
    /// makes every minified sRGB texture too dark. Raw, the bytes average. Alpha is never sRGB.
    #[test]
    fn srgb_averages_in_linear_light_and_raw_averages_the_bytes() {
        let px = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let srgb = chain(&px, 2, 2, PixelFormat::Rgba8Unorm, &SRGB);
        assert_eq!(srgb[0], [188, 188, 188, 255]);
        let raw = chain(&px, 2, 2, PixelFormat::Rgba8Unorm, &RAW);
        assert_eq!(raw[0], [128, 128, 128, 255]);
    }

    /// A flat colour survives every level unchanged in both modes: a wrong rounding or a wrong
    /// colour space shows up here first.
    #[test]
    fn a_flat_colour_is_kept_in_both_modes() {
        let px: Vec<u8> = std::iter::repeat_n([64u8, 128, 200, 255], 16)
            .flatten()
            .collect();
        for opts in [SRGB, RAW] {
            for level in chain(&px, 4, 4, PixelFormat::Rgba8Unorm, &opts) {
                for texel in level.as_chunks::<4>().0 {
                    assert_eq!(texel, &[64, 128, 200, 255], "srgb={}", opts.srgb);
                }
            }
        }
    }

    /// Integer samples round to nearest exactly, ties up, at both depths.
    #[test]
    fn integer_samples_round_ties_up() {
        // 0 + 0 + 1 + 1 = 2, a mean of 0.5.
        let px = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1];
        assert_eq!(
            chain(&px, 2, 2, PixelFormat::Rgba8Unorm, &RAW)[0],
            [1, 1, 1, 1]
        );
        let px: Vec<u8> = [0u16, 0, 1, 1]
            .iter()
            .flat_map(|v| std::iter::repeat_n(v.to_ne_bytes(), 4).flatten())
            .collect();
        let level = &chain(&px, 2, 2, PixelFormat::Rgba16Unorm, &SRGB)[0];
        assert_eq!(level.as_chunks::<2>().0, [1u16.to_ne_bytes(); 4]);
    }

    /// Floats average exactly, at both widths.
    #[test]
    fn float_levels_average_exactly() {
        let f32s: Vec<u8> = [1.0f32, 3.0, 5.0, 7.0]
            .iter()
            .flat_map(|v| std::iter::repeat_n(v.to_ne_bytes(), 4).flatten())
            .collect();
        let level = &chain(&f32s, 2, 2, PixelFormat::Rgba32Float, &SRGB)[0];
        assert_eq!(level.as_chunks::<4>().0, [4.0f32.to_ne_bytes(); 4]);
        let f16s: Vec<u8> = [1.0f32, 3.0, 5.0, 7.0]
            .iter()
            .flat_map(|v| {
                std::iter::repeat_n(f16::from_f32(*v).to_bits().to_ne_bytes(), 4).flatten()
            })
            .collect();
        let level = &chain(&f16s, 2, 2, PixelFormat::Rgba16Float, &SRGB)[0];
        assert_eq!(
            level.as_chunks::<2>().0,
            [f16::from_f32(4.0).to_bits().to_ne_bytes(); 4]
        );
    }

    /// The table encode rounds as `f32::round` does, so it agrees with 3D Review's builder.
    #[test]
    fn round_half_up_is_f32_round_for_the_filters_values() {
        for i in 0..=4095 * 64 {
            let x = i as f32 / 64.0;
            assert_eq!(round_half_up(x), x.round() as usize, "{x}");
            let next = f32::from_bits(x.to_bits() + 1);
            assert_eq!(round_half_up(next), next.round() as usize, "{next}");
            if x > 0.0 {
                let prev = f32::from_bits(x.to_bits() - 1);
                assert_eq!(round_half_up(prev), prev.round() as usize, "{prev}");
            }
        }
    }

    /// The thread split changes nothing in the output, whatever the budget lends.
    #[test]
    fn the_thread_split_changes_nothing() {
        let (w, h) = (700, 530);
        let px = noise(w * h * 4, 7);
        let mut one = MipOptions::new(true);
        one.max_threads = 1;
        for srgb in [true, false] {
            one.srgb = srgb;
            let mut any = MipOptions::new(srgb);
            any.max_threads = 0;
            assert_eq!(
                chain(&px, w as u32, h as u32, PixelFormat::Rgba8Unorm, &one),
                chain(&px, w as u32, h as u32, PixelFormat::Rgba8Unorm, &any),
            );
        }
    }

    /// A full chain from the file is taken as given: an authored chain's levels are *not* what a
    /// box filter would make.
    #[test]
    fn complete_adopts_a_full_chain_untouched() {
        let stored = vec![solid(2, 2, 200), solid(1, 1, 250)];
        let chain = complete(
            stored.clone(),
            &solid(4, 4, 10),
            4,
            4,
            PixelFormat::Rgba8Unorm,
            &SRGB,
        );
        assert_eq!(chain, stored);
    }

    /// A chain the file stopped short is padded from its last level, not from level 0.
    #[test]
    fn complete_pads_a_partial_chain_from_the_last_stored_level() {
        // Level 0 is black, the file's level 1 white: level 2 must come out white.
        let chain = complete(
            vec![solid(2, 2, 255)],
            &solid(4, 4, 0),
            4,
            4,
            PixelFormat::Rgba8Unorm,
            &SRGB,
        );
        assert_eq!(chain, [solid(2, 2, 255), solid(1, 1, 255)]);
    }

    /// A level whose length does not match its dimensions ends the adopted run.
    #[test]
    fn complete_stops_adopting_at_a_wrong_sized_level() {
        let stored = vec![solid(2, 2, 255), vec![1u8; 3]];
        let chain = complete(
            stored,
            &solid(4, 4, 0),
            4,
            4,
            PixelFormat::Rgba8Unorm,
            &SRGB,
        );
        assert_eq!(chain, [solid(2, 2, 255), solid(1, 1, 255)]);
    }

    /// Levels past the end of the chain, or past `max_levels`, are dropped rather than uploaded.
    #[test]
    fn complete_drops_levels_past_the_chain() {
        let stored = vec![solid(1, 1, 9); 3];
        assert_eq!(
            complete(
                stored,
                &solid(2, 2, 0),
                2,
                2,
                PixelFormat::Rgba8Unorm,
                &SRGB
            )
            .len(),
            1
        );
        let mut capped = MipOptions::new(true);
        capped.max_levels = 2;
        let stored = vec![solid(2, 2, 9), solid(1, 1, 9)];
        assert_eq!(
            complete(
                stored,
                &solid(4, 4, 0),
                4,
                4,
                PixelFormat::Rgba8Unorm,
                &capped
            ),
            [solid(2, 2, 9)]
        );
    }

    /// With nothing stored, `complete` is `chain`; a short level 0 gets no chain at all.
    #[test]
    fn complete_with_nothing_stored_is_chain() {
        let level0 = noise(4 * 4 * 4, 3);
        assert_eq!(
            complete(Vec::new(), &level0, 4, 4, PixelFormat::Rgba8Unorm, &SRGB),
            chain(&level0, 4, 4, PixelFormat::Rgba8Unorm, &SRGB)
        );
        assert!(complete(Vec::new(), &[0u8; 3], 4, 4, PixelFormat::Rgba8Unorm, &SRGB).is_empty());
    }

    /// The stored count is exactly the run `complete` adopts.
    #[test]
    fn stored_levels_counts_only_what_complete_adopts() {
        let f = PixelFormat::Rgba8Unorm;
        assert_eq!(stored_levels(&[], 4, 4, f, &SRGB), 0);
        assert_eq!(
            stored_levels(&[solid(2, 2, 0), solid(1, 1, 0)], 4, 4, f, &SRGB),
            2
        );
        assert_eq!(stored_levels(&[solid(2, 2, 0)], 4, 4, f, &SRGB), 1);
        assert_eq!(
            stored_levels(&[solid(2, 2, 0), vec![1u8; 3]], 4, 4, f, &SRGB),
            1
        );
        assert_eq!(
            stored_levels(&[vec![1u8; 3], solid(1, 1, 0)], 4, 4, f, &SRGB),
            0
        );
        assert_eq!(stored_levels(&vec![solid(1, 1, 0); 3], 2, 2, f, &SRGB), 1);
    }

    /// `fit` stops at the first level inside the bound, which is that level of the chain, and
    /// leaves an image that already fits alone.
    #[test]
    fn fit_is_the_first_level_of_the_chain_inside_the_bound() {
        let (w, h) = (1000, 300);
        let px = noise(w * h * 4, 11);
        for opts in [SRGB, RAW] {
            let (pixels, fw, fh) =
                fit(&px, w as u32, h as u32, PixelFormat::Rgba8Unorm, 256, &opts).unwrap();
            assert_eq!((fw, fh), (250, 75));
            let chain = chain(&px, w as u32, h as u32, PixelFormat::Rgba8Unorm, &opts);
            assert_eq!(pixels, chain[1]);
        }
        assert!(
            fit(
                &px,
                w as u32,
                h as u32,
                PixelFormat::Rgba8Unorm,
                1000,
                &SRGB
            )
            .is_none()
        );
    }
}
