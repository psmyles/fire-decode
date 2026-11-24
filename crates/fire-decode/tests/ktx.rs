//! End-to-end decode tests for KTX2, driving files built in memory through the public `decode`
//! and `probe` entry points.
//!
//! Each file is laid out by a small builder here, not by a KTX tool: a test says which header
//! fields, which level bytes and which supercompression produced which pixels. The data format
//! descriptor comes from the `ktx2` crate's own generator, a Zstandard level is a frame of raw
//! blocks written by hand, and a zlib level is `miniz_oxide`'s. Real encoder output (ETC1S,
//! UASTC, ASTC from an encoder) is `tests/ktx_fixtures.rs`'s.
#![cfg(feature = "ktx")]

use fire_decode::{
    DecodeError, DecodeOptions, DecodedImage, Format, Limits, PixelFormat, SheetKind, Surfaces,
    decode, probe, sniff,
};

// --- the builder ---------------------------------------------------------------------------

const ZSTD: u32 = 2;
const ZLIB: u32 = 3;

/// A KTX2 file, field by field. `levels` are stored bytes, level 0 first (the file writes them
/// smallest first, as the specification asks); `inflated` overrides a level's declared
/// uncompressed length.
#[derive(Clone)]
struct Ktx2 {
    vk: u32,
    width: u32,
    height: u32,
    depth: u32,
    layers: u32,
    faces: u32,
    level_count: u32,
    scheme: u32,
    levels: Vec<Vec<u8>>,
    inflated: Vec<Option<u64>>,
    kvd: Vec<(&'static str, Vec<u8>)>,
    premultiplied: bool,
}

impl Ktx2 {
    fn new(vk: u32, width: u32, height: u32, level0: Vec<u8>) -> Self {
        Ktx2 {
            vk,
            width,
            height,
            depth: 0,
            layers: 0,
            faces: 1,
            level_count: 1,
            scheme: 0,
            levels: vec![level0],
            inflated: vec![None],
            kvd: Vec::new(),
            premultiplied: false,
        }
    }

    fn mips(mut self, levels: Vec<Vec<u8>>) -> Self {
        self.level_count = levels.len() as u32 + 1;
        self.inflated.resize(levels.len() + 1, None);
        self.levels.extend(levels);
        self
    }

    fn key(mut self, k: &'static str, v: &[u8]) -> Self {
        self.kvd.push((k, v.to_vec()));
        self
    }

    /// Supercompress every level with `scheme`, declaring each one's true inflated length.
    fn compressed(mut self, scheme: u32) -> Self {
        self.scheme = scheme;
        for (i, l) in self.levels.iter_mut().enumerate() {
            self.inflated[i] = Some(l.len() as u64);
            *l = match scheme {
                ZSTD => zstd_frame(l),
                _ => miniz_oxide::deflate::compress_to_vec_zlib(l, 6),
            };
        }
        self
    }

    fn bytes(&self) -> Vec<u8> {
        let n = self.levels.len();
        let index_end = 80 + 24 * n;

        // The DFD: its total length, then one basic block for the format (an RGBA8 one for a
        // format the generator doesn't know, which is all `Reader` needs to parse).
        let format = ktx2::Format::new(self.vk).unwrap_or(ktx2::Format::R8G8B8A8_UNORM);
        let (basic, type_size) =
            ktx2::dfd::Basic::from_format_with(format, self.premultiplied, None, None, None)
                .or_else(|_| {
                    ktx2::dfd::Basic::from_format_with(
                        ktx2::Format::R8G8B8A8_UNORM,
                        self.premultiplied,
                        None,
                        None,
                        None,
                    )
                })
                .unwrap();
        let block = ktx2::dfd::Block::Basic(basic).to_vec();
        let mut dfd = ((4 + block.len()) as u32).to_le_bytes().to_vec();
        dfd.extend_from_slice(&block);

        let mut kvd = Vec::new();
        for (k, v) in &self.kvd {
            kvd.extend_from_slice(&((k.len() + 1 + v.len()) as u32).to_le_bytes());
            kvd.extend_from_slice(k.as_bytes());
            kvd.push(0);
            kvd.extend_from_slice(v);
            while kvd.len() % 4 != 0 {
                kvd.push(0);
            }
        }

        let dfd_off = index_end;
        let kvd_off = dfd_off + dfd.len();
        let mut out = vec![0u8; kvd_off];
        out[dfd_off..].copy_from_slice(&dfd);
        out.extend_from_slice(&kvd);
        let mut offsets = vec![0usize; n];
        for i in (0..n).rev() {
            while !out.len().is_multiple_of(16) {
                out.push(0);
            }
            offsets[i] = out.len();
            out.extend_from_slice(&self.levels[i]);
        }
        // One spare byte, so no section ends exactly at the end of the file (which `ktx2`'s
        // bounds checks read as running past it).
        out.push(0);

        let mut h = Vec::with_capacity(80);
        h.extend_from_slice(&ktx2::MAGIC);
        for v in [
            self.vk,
            if self.vk == 0 { 1 } else { type_size },
            self.width,
            self.height,
            self.depth,
            self.layers,
            self.faces,
            self.level_count,
            self.scheme,
            dfd_off as u32,
            dfd.len() as u32,
            if kvd.is_empty() { 0 } else { kvd_off as u32 },
            kvd.len() as u32,
        ] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.extend_from_slice(&0u64.to_le_bytes()); // sgd offset
        h.extend_from_slice(&0u64.to_le_bytes()); // sgd length
        out[..80].copy_from_slice(&h);
        for (i, offset) in offsets.iter().enumerate() {
            let at = 80 + 24 * i;
            let stored = self.levels[i].len() as u64;
            let inflated = self.inflated[i].unwrap_or(stored);
            out[at..at + 8].copy_from_slice(&(*offset as u64).to_le_bytes());
            out[at + 8..at + 16].copy_from_slice(&stored.to_le_bytes());
            out[at + 16..at + 24].copy_from_slice(&inflated.to_le_bytes());
        }
        out
    }
}

/// A Zstandard frame of raw (stored) blocks: the magic, a frame header with a 1 MiB window and
/// no content size or checksum, then `data` in blocks of at most 128 KiB, the last one flagged.
fn zstd_frame(data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x50];
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(128 << 10).collect()
    };
    for (i, c) in chunks.iter().enumerate() {
        let last = u32::from(i + 1 == chunks.len());
        let header = last | ((c.len() as u32) << 3); // block type 0: raw
        f.extend_from_slice(&header.to_le_bytes()[..3]);
        f.extend_from_slice(c);
    }
    f
}

// --- VkFormat values the tests use ---------------------------------------------------------

const R8_UNORM: u32 = 9;
const R8G8B8_UNORM: u32 = 23;
const R8G8B8A8_UNORM: u32 = 37;
const R8G8B8A8_UINT: u32 = 41;
const R8G8B8A8_SRGB: u32 = 43;
const B8G8R8A8_UNORM: u32 = 44;
const R5G6B5_UNORM_PACK16: u32 = 4;
const R16G16B16A16_SFLOAT: u32 = 97;
const BC1_RGB_UNORM_BLOCK: u32 = 131;
const BC1_RGBA_UNORM_BLOCK: u32 = 133;
const BC7_UNORM_BLOCK: u32 = 145;
const ETC2_R8G8B8_UNORM_BLOCK: u32 = 147;
const ETC2_R8G8B8A8_UNORM_BLOCK: u32 = 151;
const EAC_R11_UNORM_BLOCK: u32 = 153;
const EAC_R11_SNORM_BLOCK: u32 = 154;
const EAC_R11G11_UNORM_BLOCK: u32 = 155;
const ASTC_4X4_UNORM_BLOCK: u32 = 157;
const ASTC_6X6_UNORM_BLOCK: u32 = 165;
const ASTC_8X5_SRGB_BLOCK: u32 = 168;
const ASTC_4X4_SFLOAT_BLOCK: u32 = 1_000_066_000;
const ASTC_5X5_SFLOAT_BLOCK: u32 = 1_000_066_002;
const PVRTC1_4BPP_UNORM_BLOCK: u32 = 1_000_054_001;
const PVRTC2_4BPP_UNORM_BLOCK: u32 = 1_000_054_003;

// --- helpers -------------------------------------------------------------------------------

fn opts() -> DecodeOptions {
    DecodeOptions::viewer()
}

fn rgba8(img: &DecodedImage, i: usize) -> [u8; 4] {
    img.pixels[i * 4..i * 4 + 4].try_into().unwrap()
}

fn lanes16(img: &DecodedImage, i: usize) -> [u16; 4] {
    let px: &[u16] = bytemuck::cast_slice(&img.pixels);
    px[i * 4..i * 4 + 4].try_into().unwrap()
}

/// `w`x`h` RGBA8 texels, each its own index's colour: `(i, i >> 8, 7, 255)`.
fn ramp(w: u32, h: u32) -> Vec<u8> {
    (0..w * h)
        .flat_map(|i| [i as u8, (i >> 8) as u8, 7, 255])
        .collect()
}

/// One BC1 block of a single RGB565 colour.
fn bc1_solid(colour: u16) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0..2].copy_from_slice(&colour.to_le_bytes());
    b
}

/// An ASTC LDR void-extent block: every texel `rgba`, whatever the footprint.
fn astc_solid(rgba: [u8; 4]) -> [u8; 16] {
    let mut b = [0xFFu8; 16];
    b[0] = 0xFC;
    b[1] = 0xFD; // void extent, LDR
    for (c, v) in rgba.iter().enumerate() {
        b[8 + c * 2] = *v;
        b[9 + c * 2] = *v;
    }
    b
}

/// An ETC1 / ETC2 block in individual mode, both halves the 4-bit colour `(r, g, b)`, every
/// texel's modifier the smallest positive one (+2).
fn etc_solid(r: u8, g: u8, b: u8) -> [u8; 8] {
    [r << 4 | r, g << 4 | g, b << 4 | b, 0, 0, 0, 0, 0]
}

/// An EAC block whose sixteen values all decode to the 8-bit `base`: multiplier 0 (read as 1)
/// and the first table's first modifier, -3, which the decoder's rounding cancels.
fn eac_solid(base: u8) -> [u8; 8] {
    [base, 0, 0, 0, 0, 0, 0, 0]
}

fn malformed(r: Result<DecodedImage, DecodeError>) -> String {
    match r {
        Err(DecodeError::Malformed(m)) => m,
        other => panic!("expected Malformed, got {other:?}"),
    }
}

fn too_large(r: Result<DecodedImage, DecodeError>) -> String {
    match r {
        Err(DecodeError::TooLarge(m)) => m,
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

// --- routing -------------------------------------------------------------------------------

/// The twelve-byte identifier routes it, whatever the extension says, and probe agrees with
/// decode on the size.
#[test]
fn ktx2_routes_by_its_identifier_not_its_extension() {
    let file = Ktx2::new(R8G8B8A8_UNORM, 2, 2, ramp(2, 2)).bytes();
    assert_eq!(sniff(&file, None), Format::Ktx2);
    assert_eq!(sniff(&file, Some("png")), Format::Ktx2);
    for hint in [None, Some("png"), Some("ktx2")] {
        let img = decode(&file, hint, &opts()).expect("routes by magic");
        assert_eq!(img.source.format, Format::Ktx2);
        assert_eq!(img.source.name, "KTX2 RGBA8");
    }
    let p = probe(std::io::Cursor::new(&file), None).expect("probes");
    assert_eq!((p.width, p.height, p.format), (2, 2, Format::Ktx2));
}

// --- uncompressed --------------------------------------------------------------------------

/// The plain layouts read their bytes in `VkFormat`'s component order, and the packed ones from
/// the most significant bits down.
#[test]
fn uncompressed_layouts_come_out_rgba() {
    let img = decode(
        &Ktx2::new(R8G8B8A8_UNORM, 2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).bytes(),
        None,
        &opts(),
    )
    .expect("RGBA8");
    assert_eq!(
        (rgba8(&img, 0), rgba8(&img, 1)),
        ([1, 2, 3, 4], [5, 6, 7, 8])
    );

    let img = decode(
        &Ktx2::new(B8G8R8A8_UNORM, 1, 1, vec![1, 2, 3, 4]).bytes(),
        None,
        &opts(),
    )
    .expect("BGRA8");
    assert_eq!(rgba8(&img, 0), [3, 2, 1, 4]);
    assert_eq!(img.source.name, "KTX2 BGRA8");

    let img = decode(
        &Ktx2::new(R8G8B8_UNORM, 1, 1, vec![9, 8, 7]).bytes(),
        None,
        &opts(),
    )
    .expect("RGB8");
    assert_eq!(rgba8(&img, 0), [9, 8, 7, 255]);
    assert_eq!(img.source.channels, 3);

    let img = decode(&Ktx2::new(R8_UNORM, 1, 1, vec![99]).bytes(), None, &opts()).expect("R8");
    assert_eq!(rgba8(&img, 0), [99, 99, 99, 255]);
    assert_eq!(img.source.channels, 1);

    // Pure red in R5G6B5 is the top five bits.
    let img = decode(
        &Ktx2::new(R5G6B5_UNORM_PACK16, 1, 1, 0xf800u16.to_le_bytes().to_vec()).bytes(),
        None,
        &opts(),
    )
    .expect("R5G6B5");
    assert_eq!(rgba8(&img, 0), [255, 0, 0, 255]);

    let halves: [u16; 4] = [0x3c00, 0x3800, 0x4000, 0x3c00];
    let data = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
    let img = decode(
        &Ktx2::new(R16G16B16A16_SFLOAT, 1, 1, data).bytes(),
        None,
        &opts(),
    )
    .expect("RGBA16F");
    assert_eq!(img.format, PixelFormat::Rgba16Float);
    assert_eq!(lanes16(&img, 0), halves);
}

/// Integer formats have no display mapping, and the error names the format.
#[test]
fn an_integer_format_is_refused_by_name() {
    let m = malformed(decode(
        &Ktx2::new(R8G8B8A8_UINT, 1, 1, vec![0; 4]).bytes(),
        None,
        &opts(),
    ));
    assert!(m.contains("R8G8B8A8_UINT"), "{m}");
    // probe refuses it the same way.
    let file = Ktx2::new(R8G8B8A8_UINT, 1, 1, vec![0; 4]).bytes();
    assert!(probe(std::io::Cursor::new(&file), None).is_err());
}

// --- block codecs --------------------------------------------------------------------------

/// The BC formats decode as DDS's do. Vulkan's `BC1_RGB` is the one difference: the third mode's
/// last index is opaque black there, where DDS (and `BC1_RGBA`) read it as transparent.
#[test]
fn bc1_rgb_is_opaque_and_bc1_rgba_is_not() {
    // c0 == c1 selects the three-colour mode; every index 3.
    let block = [0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff].to_vec();
    let rgb = decode(
        &Ktx2::new(BC1_RGB_UNORM_BLOCK, 4, 4, block.clone()).bytes(),
        None,
        &opts(),
    )
    .unwrap();
    assert_eq!(rgba8(&rgb, 0), [0, 0, 0, 255]);
    assert_eq!((rgb.source.name, rgb.source.channels), ("KTX2 BC1", 3));
    let rgba = decode(
        &Ktx2::new(BC1_RGBA_UNORM_BLOCK, 4, 4, block).bytes(),
        None,
        &opts(),
    )
    .unwrap();
    assert_eq!(rgba8(&rgba, 0), [0, 0, 0, 0]);
    assert_eq!(rgba.source.channels, 4);

    let red = decode(
        &Ktx2::new(BC1_RGBA_UNORM_BLOCK, 4, 4, bc1_solid(0xf800).to_vec()).bytes(),
        None,
        &opts(),
    )
    .unwrap();
    assert_eq!(rgba8(&red, 15), [255, 0, 0, 255]);

    let bc7 = decode(
        &Ktx2::new(BC7_UNORM_BLOCK, 4, 4, vec![0x40; 16]).bytes(),
        None,
        &opts(),
    )
    .unwrap();
    assert_eq!(bc7.source.name, "KTX2 BC7");
}

/// ETC1-mode blocks decode the same through ETC2 RGB (ETC2 reads ETC1 as a subset), and ETC2's
/// EAC alpha block with a zero multiplier is its base value throughout.
#[test]
fn etc2_blocks_decode() {
    let img = decode(
        &Ktx2::new(ETC2_R8G8B8_UNORM_BLOCK, 4, 4, etc_solid(15, 0, 0).to_vec()).bytes(),
        None,
        &opts(),
    )
    .expect("ETC2 RGB");
    assert_eq!(img.source.name, "KTX2 ETC2 RGB");
    assert_eq!(img.source.channels, 3);
    for i in 0..16 {
        assert_eq!(rgba8(&img, i), [255, 2, 2, 255], "texel {i}");
    }

    let mut rgba = eac_solid(128).to_vec();
    rgba.extend_from_slice(&etc_solid(0, 15, 0));
    let img = decode(
        &Ktx2::new(ETC2_R8G8B8A8_UNORM_BLOCK, 4, 4, rgba).bytes(),
        None,
        &opts(),
    )
    .expect("ETC2 RGBA");
    assert_eq!(rgba8(&img, 5), [2, 255, 2, 128]);
}

/// EAC R11 is one channel, shown grey like BC4; RG11 lands in red and green like BC5; the
/// signed spelling is re-centred, zero on mid-grey. The status bar reports the 11 bits stored.
#[test]
fn eac_channels_land_like_bc4_and_bc5() {
    let r = decode(
        &Ktx2::new(EAC_R11_UNORM_BLOCK, 4, 4, eac_solid(200).to_vec()).bytes(),
        None,
        &opts(),
    )
    .expect("EAC R11");
    assert_eq!(rgba8(&r, 0), [200, 200, 200, 255]);
    assert_eq!((r.source.channels, r.source.bit_depth), (1, 11));

    let mut rg = eac_solid(10).to_vec();
    rg.extend_from_slice(&eac_solid(250));
    let rg = decode(
        &Ktx2::new(EAC_R11G11_UNORM_BLOCK, 4, 4, rg).bytes(),
        None,
        &opts(),
    )
    .expect("RG11");
    assert_eq!(rgba8(&rg, 0), [10, 250, 0, 255]);

    let s = decode(
        &Ktx2::new(EAC_R11_SNORM_BLOCK, 4, 4, eac_solid(0).to_vec()).bytes(),
        None,
        &opts(),
    )
    .expect("signed R11");
    assert_eq!(rgba8(&s, 0), [127, 127, 127, 255]);
}

/// ASTC's footprint comes from the `VkFormat`; a void-extent block fills it, and a surface that
/// isn't a whole number of blocks is cropped.
#[test]
fn astc_footprints_and_cropping() {
    let img = decode(
        &Ktx2::new(
            ASTC_4X4_UNORM_BLOCK,
            4,
            4,
            astc_solid([10, 20, 30, 40]).to_vec(),
        )
        .bytes(),
        None,
        &opts(),
    )
    .expect("ASTC 4x4");
    assert_eq!(img.source.name, "KTX2 ASTC 4x4");
    assert_eq!(rgba8(&img, 15), [10, 20, 30, 40]);

    // 6x6 blocks over a 7x5 surface: 2x1 blocks, cropped to 7x5.
    let mut data = astc_solid([1, 2, 3, 255]).to_vec();
    data.extend_from_slice(&astc_solid([4, 5, 6, 255]));
    let img = decode(
        &Ktx2::new(ASTC_6X6_UNORM_BLOCK, 7, 5, data).bytes(),
        None,
        &opts(),
    )
    .expect("ASTC 6x6");
    assert_eq!((img.width, img.height), (7, 5));
    assert_eq!(rgba8(&img, 5), [1, 2, 3, 255]);
    assert_eq!(rgba8(&img, 6), [4, 5, 6, 255]);
    assert_eq!(rgba8(&img, 4 * 7 + 6), [4, 5, 6, 255]);

    let img = decode(
        &Ktx2::new(ASTC_8X5_SRGB_BLOCK, 8, 5, astc_solid([9, 9, 9, 9]).to_vec()).bytes(),
        None,
        &opts(),
    )
    .expect("ASTC 8x5 sRGB");
    assert_eq!(img.source.name, "KTX2 ASTC 8x5");
}

/// ASTC HDR goes to the Basis transcoder, which reads UASTC HDR 4x4 and ASTC HDR 6x6 only: a
/// plain ASTC HDR file is refused, not shown as garbage.
#[test]
fn astc_hdr_outside_basis_is_refused() {
    for vk in [ASTC_4X4_SFLOAT_BLOCK, ASTC_5X5_SFLOAT_BLOCK] {
        let file = Ktx2::new(vk, 4, 4, vec![0; 16]).bytes();
        let m = malformed(decode(&file, None, &opts()));
        assert!(m.contains("SFLOAT"), "{m}");
    }
}

/// PVRTC1 decodes a whole surface at a time, at power-of-two sizes; a level smaller than two
/// blocks a side is stored padded to that, and the padding is cropped off. Any other size is
/// refused before anything is decoded.
#[test]
fn pvrtc1_needs_power_of_two_and_crops_its_padding() {
    // 8x8 at 4bpp is 2x2 blocks; its 4x4 mip is stored as 2x2 blocks too.
    let file = Ktx2::new(PVRTC1_4BPP_UNORM_BLOCK, 8, 8, vec![0x55; 32]).mips(vec![vec![0x55; 32]]);
    let img = decode(&file.bytes(), None, &opts()).expect("PVRTC1");
    assert_eq!(img.source.name, "KTX2 PVRTC1 4bpp");
    assert_eq!((img.width, img.height), (8, 8));
    let mips = img.source_mips.expect("its 4x4 mip");
    assert_eq!(mips[0].len(), 4 * 4 * 4);

    let m = malformed(decode(
        &Ktx2::new(PVRTC1_4BPP_UNORM_BLOCK, 12, 8, vec![0; 48]).bytes(),
        None,
        &opts(),
    ));
    assert!(m.contains("power-of-two"), "{m}");
    let m = malformed(decode(
        &Ktx2::new(PVRTC2_4BPP_UNORM_BLOCK, 8, 8, vec![0; 32]).bytes(),
        None,
        &opts(),
    ));
    assert!(m.contains("PVRTC2"), "{m}");
}

// --- sheets --------------------------------------------------------------------------------

/// Six faces, one colour each, in the `+X -X +Y -Y +Z -Z` order KTX2 shares with DDS; with
/// `Surfaces::First` just `+X`.
#[test]
fn a_cubemap_tiles_its_faces_in_order() {
    let faces: Vec<u8> = (0..6u8)
        .flat_map(|f| [f * 40, 0, 0, 255].repeat(4))
        .collect();
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 2, 2, faces);
    k.faces = 6;
    let file = k.bytes();
    let img = decode(&file, None, &opts()).expect("cubemap");
    let l = img.layout.expect("a sheet");
    assert_eq!(
        (l.cols, l.rows, l.frames, l.kind),
        (3, 2, 6, SheetKind::CubeFaces)
    );
    assert_eq!((img.width, img.height), (6, 4));
    for f in 0..6u32 {
        let (x, y) = ((f % 3) * 2, (f / 3) * 2);
        assert_eq!(
            rgba8(&img, (y * 6 + x) as usize)[0],
            f as u8 * 40,
            "face {f}"
        );
    }
    let p = probe(std::io::Cursor::new(&file), None).unwrap();
    assert_eq!((p.width, p.height), (6, 4));

    let first = DecodeOptions {
        surfaces: Surfaces::First,
        ..opts()
    };
    let img = decode(&file, None, &first).expect("first face");
    assert_eq!((img.width, img.height, img.layout), (2, 2, None));
    assert_eq!(rgba8(&img, 0)[0], 0);
}

/// Two layers of a cube array are twelve faces, layer by layer, on a 4x3 grid.
#[test]
fn a_cube_array_is_layer_major() {
    let data: Vec<u8> = (0..12u8).flat_map(|f| [f, 0, 0, 255]).collect();
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 1, 1, data);
    (k.faces, k.layers) = (6, 2);
    let img = decode(&k.bytes(), None, &opts()).expect("cube array");
    let l = img.layout.unwrap();
    assert_eq!(
        (l.cols, l.rows, l.frames, l.kind),
        (4, 3, 12, SheetKind::CubeFaces)
    );
    for f in 0..12 {
        assert_eq!(rgba8(&img, f)[0], f as u8);
    }
}

/// Five layers on a 3x2 grid leave the sixth cell transparent black.
#[test]
fn an_array_leaves_its_spare_cell_empty() {
    let data: Vec<u8> = (1..=5u8).flat_map(|f| [f, f, f, 255]).collect();
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 1, 1, data);
    k.layers = 5;
    let img = decode(&k.bytes(), None, &opts()).expect("array");
    let l = img.layout.unwrap();
    assert_eq!(
        (l.cols, l.rows, l.frames, l.kind),
        (3, 2, 5, SheetKind::ArrayLayers)
    );
    assert_eq!(rgba8(&img, 4), [5, 5, 5, 255]);
    assert_eq!(rgba8(&img, 5), [0, 0, 0, 0]);
}

/// A volume's slices tile like layers. Its levels are volumes, with fewer slices each, so the
/// sheet keeps no chain, and its first slice alone keeps none either.
#[test]
fn a_volume_tiles_its_slices_and_keeps_no_chain() {
    let slices: Vec<u8> = (0..4u8)
        .flat_map(|z| [z * 50, 0, 0, 255].repeat(4))
        .collect();
    let mut k =
        Ktx2::new(R8G8B8A8_UNORM, 2, 2, slices).mips(vec![vec![7, 0, 0, 255, 8, 0, 0, 255]]);
    k.depth = 4;
    let img = decode(&k.bytes(), None, &opts()).expect("volume");
    let l = img.layout.unwrap();
    assert_eq!(
        (l.cols, l.rows, l.frames, l.kind),
        (2, 2, 4, SheetKind::VolumeSlices)
    );
    assert_eq!(rgba8(&img, 2 * 4 + 2)[0], 150);
    assert!(img.source_mips.is_none());
    let first = DecodeOptions {
        surfaces: Surfaces::First,
        ..opts()
    };
    let img = decode(&k.bytes(), None, &first).unwrap();
    assert!(img.source_mips.is_none());
}

// --- mips ----------------------------------------------------------------------------------

/// The file stores its levels smallest first, behind an index that lists them largest first: the
/// chain comes back in the index's order, each level its own size.
#[test]
fn stored_mips_come_back_largest_first() {
    let file = Ktx2::new(R8G8B8A8_UNORM, 4, 2, [10, 0, 0, 255].repeat(8))
        .mips(vec![[20, 0, 0, 255].repeat(2), [30, 0, 0, 255].to_vec()]);
    let img = decode(&file.bytes(), None, &opts()).unwrap();
    let mips = img.source_mips.expect("a chain");
    assert_eq!(mips.len(), 2);
    assert_eq!((mips[0].len(), mips[0][0]), (2 * 4, 20));
    assert_eq!((mips[1].len(), mips[1][0]), (4, 30));
    assert_eq!(img.pixels[0], 10);

    // inspect() keeps none; a level count of 0 ("generate them") is one stored level; a count
    // past what the size can halve to stops at 1x1.
    assert!(
        decode(&file.bytes(), None, &DecodeOptions::inspect())
            .unwrap()
            .source_mips
            .is_none()
    );
    let mut zero = Ktx2::new(R8G8B8A8_UNORM, 2, 2, ramp(2, 2));
    zero.level_count = 0;
    let img = decode(&zero.bytes(), None, &opts()).expect("level count 0");
    assert!(img.source_mips.is_none());
    let mut many = file.clone();
    many.level_count = 40;
    many.levels.resize(40, vec![0; 4]);
    many.inflated.resize(40, None);
    let img = decode(&many.bytes(), None, &opts()).expect("too many levels");
    assert_eq!(img.source_mips.unwrap().len(), 2);
}

// --- supercompression ----------------------------------------------------------------------

/// Zstandard and zlib levels inflate to exactly what they declare, mips included.
#[test]
fn zstd_and_zlib_levels_inflate() {
    let plain = Ktx2::new(R8G8B8A8_UNORM, 4, 4, ramp(4, 4)).mips(vec![ramp(2, 2), ramp(1, 1)]);
    let reference = decode(&plain.bytes(), None, &opts()).unwrap();
    for scheme in [ZSTD, ZLIB] {
        let img = decode(&plain.clone().compressed(scheme).bytes(), None, &opts())
            .unwrap_or_else(|e| panic!("scheme {scheme}: {e}"));
        assert_eq!(img.pixels, reference.pixels, "scheme {scheme}");
        assert_eq!(img.source_mips, reference.source_mips, "scheme {scheme}");
    }
}

/// A level that declares an inflated length its dimensions don't imply is refused before it is
/// inflated, and one that inflates to more or less than it declares is refused after.
#[test]
fn a_supercompressed_level_must_inflate_to_what_it_declares() {
    let mut lying = Ktx2::new(R8G8B8A8_UNORM, 4, 4, ramp(4, 4)).compressed(ZSTD);
    lying.inflated[0] = Some(1 << 40);
    let m = malformed(decode(&lying.bytes(), None, &opts()));
    assert!(m.contains("level 0 says it inflates"), "{m}");

    for scheme in [ZSTD, ZLIB] {
        // A stream holding one texel more than the level, then one fewer, declared as the level.
        for len in [4 * 4 * 4 + 4, 4 * 4 * 4 - 4] {
            let mut k = Ktx2::new(R8G8B8A8_UNORM, 4, 4, vec![1; len]).compressed(scheme);
            k.inflated[0] = Some(4 * 4 * 4);
            let m = malformed(decode(&k.bytes(), None, &opts()));
            assert!(m.contains("doesn't inflate"), "scheme {scheme}, {len}: {m}");
        }
    }

    // A mip that fails the same checks ends the chain instead.
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 2, 2, ramp(2, 2))
        .mips(vec![ramp(1, 1)])
        .compressed(ZSTD);
    k.inflated[1] = Some(99);
    let img = decode(&k.bytes(), None, &opts()).expect("level 0 is fine");
    assert!(img.source_mips.is_none());
}

/// A level inflates whole — every layer of it — so the guard has to bound the inflated level,
/// not just the surface returned: a deep volume decoded as its first slice is still refused.
#[test]
fn a_supercompressed_level_past_the_guard_is_refused() {
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 256, 256, vec![0; 16]).compressed(ZSTD);
    k.depth = 256;
    k.inflated[0] = Some(256 * 256 * 256 * 4);
    let first = DecodeOptions {
        surfaces: Surfaces::First,
        limits: Limits {
            max_dim: 1 << 16,
            max_bytes: 32 << 20,
        },
        ..opts()
    };
    let m = too_large(decode(&k.bytes(), None, &first));
    assert!(m.contains("level 0 inflates"), "{m}");
}

/// A Basis texture's declared Zstandard length is bounded by what its dimensions can hold
/// before the transcoder, which allocates it, ever sees the file.
#[test]
fn a_basis_level_claiming_more_than_its_size_holds_is_refused_first() {
    let mut k = Ktx2::new(0, 8, 8, vec![0; 64]).compressed(ZSTD);
    k.inflated[0] = Some(1 << 40);
    let m = malformed(decode(&k.bytes(), None, &opts()));
    assert!(m.contains("holds at most"), "{m}");
}

/// An unknown scheme, and BasisLZ over a real format, are named rather than guessed at.
#[test]
fn unknown_supercompression_is_refused() {
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 1, 1, vec![0; 4]);
    k.scheme = 9;
    assert!(malformed(decode(&k.bytes(), None, &opts())).contains("scheme 9"));
    k.scheme = 1;
    assert!(malformed(decode(&k.bytes(), None, &opts())).contains("BasisLZ"));
}

// --- guards and malformed files ------------------------------------------------------------

/// The guard runs on the header's claim, before anything else is read: a 100000x100000 header
/// over four bytes is refused as too large, not as truncated, and so is the sheet a cubemap of
/// faces each within the guard would tile into.
#[test]
fn a_bomb_header_is_refused_before_allocating() {
    let lim = DecodeOptions {
        limits: Limits {
            max_dim: 1 << 16,
            max_bytes: 1 << 30,
        },
        ..opts()
    };
    let m = too_large(decode(
        &Ktx2::new(R8G8B8A8_UNORM, 100_000, 100_000, vec![0; 4]).bytes(),
        None,
        &lim,
    ));
    assert!(m.contains("KTX2"), "{m}");
    let mut cube = Ktx2::new(R8G8B8A8_UNORM, 16384, 16384, vec![0; 4]);
    cube.faces = 6;
    let m = too_large(decode(&cube.bytes(), None, &lim));
    assert!(m.contains("sheet"), "{m}");
}

/// Short level data, a level index past the end of the file, and shapes KTX2 doesn't allow.
#[test]
fn malformed_files_are_errors_not_panics() {
    let m = malformed(decode(
        &Ktx2::new(R8G8B8A8_UNORM, 4, 4, vec![0; 60]).bytes(),
        None,
        &opts(),
    ));
    assert!(m.contains("truncated"), "{m}");

    // A short mip ends the chain; level 0 still decodes.
    let k = Ktx2::new(R8G8B8A8_UNORM, 2, 2, ramp(2, 2)).mips(vec![vec![0; 2]]);
    assert!(
        decode(&k.bytes(), None, &opts())
            .unwrap()
            .source_mips
            .is_none()
    );

    // The level index points past the end.
    let mut file = Ktx2::new(R8G8B8A8_UNORM, 2, 2, ramp(2, 2)).bytes();
    file[80..88].copy_from_slice(&(1u64 << 40).to_le_bytes());
    malformed(decode(&file, None, &opts()));

    for (faces, w, h, depth, layers, what) in [
        (3, 4, 4, 0, 0, "3 faces"),
        (6, 4, 2, 0, 0, "square"),
        (1, 4, 4, 2, 2, "array of volume"),
    ] {
        let mut k = Ktx2::new(R8G8B8A8_UNORM, w, h, vec![0; 1 << 12]);
        (k.faces, k.depth, k.layers) = (faces, depth, layers);
        let m = malformed(decode(&k.bytes(), None, &opts()));
        assert!(m.contains(what), "{what}: {m}");
    }

    // Every prefix of a valid file is an error or a decode, never a panic.
    let file = Ktx2::new(BC7_UNORM_BLOCK, 8, 8, vec![0x40; 64])
        .key("KTXorientation", b"ru\0")
        .bytes();
    for n in 0..file.len() {
        let _ = decode(&file[..n], None, &opts());
        let _ = probe(std::io::Cursor::new(&file[..n]), None);
    }
}

// --- orientation and alpha -----------------------------------------------------------------

/// `KTXorientation` says which way the rows run; the rows come back top first whatever the
/// setup, in every surface and every level, with the faces of a sheet left in their places.
#[test]
fn ktx_orientation_is_applied_to_every_cell_and_level() {
    let base = Ktx2::new(
        R8G8B8A8_UNORM,
        2,
        2,
        vec![1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255],
    )
    .mips(vec![vec![9, 0, 0, 255]]);
    let red = |img: &DecodedImage| (0..4).map(|i| rgba8(img, i)[0]).collect::<Vec<_>>();

    let up = base.clone().key("KTXorientation", b"ru\0");
    for setup in [opts(), DecodeOptions::texture(), DecodeOptions::inspect()] {
        assert_eq!(
            red(&decode(&up.bytes(), None, &setup).unwrap()),
            [3, 4, 1, 2]
        );
    }
    let left = base.clone().key("KTXorientation", b"ld\0");
    assert_eq!(
        red(&decode(&left.bytes(), None, &opts()).unwrap()),
        [2, 1, 4, 3]
    );
    let both = base.clone().key("KTXorientation", b"lu\0");
    let img = decode(&both.bytes(), None, &opts()).unwrap();
    assert_eq!(red(&img), [4, 3, 2, 1]);
    assert_eq!(img.source_mips.unwrap()[0][0], 9);
    assert_eq!(
        red(&decode(&base.bytes(), None, &opts()).unwrap()),
        [1, 2, 3, 4]
    );

    // Two layers, each flipped in its own cell.
    let mut arr = Ktx2::new(
        R8G8B8A8_UNORM,
        1,
        2,
        vec![1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255],
    )
    .key("KTXorientation", b"ru\0");
    arr.layers = 2;
    let img = decode(&arr.bytes(), None, &opts()).unwrap();
    // A 2x1 grid of 1x2 cells: row 0 is (2, 4), row 1 is (1, 3).
    assert_eq!(red(&img), [2, 4, 1, 3]);
}

/// The DFD's premultiplied flag straightens the colour, as DDS's alpha mode does: out of the
/// stored values for a linear format, and out of linear light for an sRGB one, where the
/// multiplying was done (64 and 32 at alpha 128 are 0.051 and 0.014 linear; halved out, 90 and
/// 47 encoded, not 128 and 64).
#[test]
fn premultiplied_alpha_is_straightened() {
    let mut k = Ktx2::new(R8G8B8A8_UNORM, 1, 1, vec![64, 32, 0, 128]);
    assert_eq!(
        rgba8(&decode(&k.bytes(), None, &opts()).unwrap(), 0),
        [64, 32, 0, 128]
    );
    k.premultiplied = true;
    assert_eq!(
        rgba8(&decode(&k.bytes(), None, &opts()).unwrap(), 0),
        [128, 64, 0, 128]
    );
    k.vk = R8G8B8A8_SRGB;
    assert_eq!(
        rgba8(&decode(&k.bytes(), None, &opts()).unwrap(), 0),
        [90, 47, 0, 128]
    );
}
