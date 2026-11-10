//! `probe()` (FD19): a file's size and format, read from its headers without decoding pixels.
//!
//! The size is what [`decode`](crate::decode) returns with
//! [`DecodeOptions::inspect`](crate::DecodeOptions::inspect): EXIF orientation off, no fit, a DDS
//! file's whole sheet. HEIF's own `irot` / `clap` are part of the image and are applied, as decode
//! applies them. Each format is read the cheapest exact way: most from a few bytes of header, JPEG
//! and HEIF by walking their markers and boxes with seeks (HEIF's `meta` box can follow `mdat`),
//! TIFF, EXR and the `image` crate's formats through their own lazy header readers. Two read the
//! whole file, because their size is only known once it is parsed: JPEG XL, and camera raw (the
//! size of the embedded preview).

use std::io::{BufReader, Read, Seek, SeekFrom};

use crate::{Backend, DecodeError, Format, HdrKind};

/// What [`probe`] reads from a file's headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub format: Format,
    /// Whether the image is HDR, and how (FD17): what an app needs to choose its
    /// [`Hdr`](crate::Hdr) before decoding. PQ and HLG come from a HEIF primary item's nclx
    /// colour box; every other format, and a HEIF without one, is `None`. Gain maps are step 6c's.
    pub hdr: HdrKind,
}

/// The size and format of the image `reader` holds, from its headers alone. The size is what
/// [`decode`](crate::decode) returns with [`DecodeOptions::inspect`](crate::DecodeOptions::inspect).
/// A format this build doesn't decode is [`DecodeError::Unsupported`], as it is for `decode`; a
/// panic is [`DecodeError::Panicked`] (FD7).
///
/// A PSD saved without a merged image still probes (its header has the size), though it won't
/// decode.
pub fn probe(mut reader: impl Read + Seek, ext_hint: Option<&str>) -> Result<Probe, DecodeError> {
    crate::guarded(|| probe_inner(&mut reader, ext_hint))
}

/// The prefix routing looks at. Enough for every fixed header and the formats sniffed from text.
const HEAD: u64 = 4096;

fn probe_inner<R: Read + Seek>(r: &mut R, ext: Option<&str>) -> Result<Probe, DecodeError> {
    let len = r.seek(SeekFrom::End(0)).map_err(io)?;
    r.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut head = vec![0u8; len.min(HEAD) as usize];
    r.read_exact(&mut head).map_err(io)?;
    r.seek(SeekFrom::Start(0)).map_err(io)?;

    let backend = crate::route(&head, len as usize, ext);
    let format = backend.format(&head, ext);
    if format != Format::Unknown && !format.is_supported() {
        return Err(DecodeError::Unsupported(format));
    }
    let short = || DecodeError::Malformed(format!("{} header is truncated", format.name()));
    let mut hdr = HdrKind::None;
    let (width, height) = match backend {
        Backend::Psd => {
            // `8BPS`, version, 6 reserved bytes, channels, then height and width.
            let be32 = |at: usize| {
                head.get(at..at + 4)
                    .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            };
            (be32(18).ok_or_else(short)?, be32(14).ok_or_else(short)?)
        }
        Backend::Exr => exr_dims(r)?,
        Backend::Heif(_) => {
            let (w, h, kind) = heif::dims(r, len)?;
            hdr = kind;
            (w, h)
        }
        Backend::Raw(_) => raw_dims(r)?,
        Backend::Dds => crate::dds::probe_dims(&head, len as usize)?,
        Backend::Tiff => {
            let mut d = tiff::decoder::Decoder::new(BufReader::new(r)).map_err(malformed)?;
            d.dimensions().map_err(malformed)?
        }
        Backend::Hdr => {
            use image::ImageDecoder;
            // Non-strict, as decode reads it: `#?RGBE` and signature-less `.pic` files too.
            image::codecs::hdr::HdrDecoder::with_strictness(BufReader::new(r), false)
                .map_err(malformed)?
                .dimensions()
        }
        Backend::Gif => image_dims(r, Some(image::ImageFormat::Gif))?,
        Backend::Png => image_dims(r, Some(image::ImageFormat::Png))?,
        Backend::Tga => image_dims(r, Some(image::ImageFormat::Tga))?,
        Backend::Cur => image_dims(r, Some(image::ImageFormat::Ico))?,
        Backend::Zune(z) => {
            use zune_image::codecs::ImageFormat as Z;
            match z {
                Z::JPEG => jpeg_dims(r)?,
                Z::BMP => image_dims(r, Some(image::ImageFormat::Bmp))?,
                Z::QOI => {
                    // `qoif`, then width and height, big-endian.
                    let be32 = |at: usize| {
                        head.get(at..at + 4)
                            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                    };
                    (be32(4).ok_or_else(short)?, be32(8).ok_or_else(short)?)
                }
                Z::Farbfeld => {
                    let be32 = |at: usize| {
                        head.get(at..at + 4)
                            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                    };
                    (be32(8).ok_or_else(short)?, be32(12).ok_or_else(short)?)
                }
                Z::PPM => {
                    use zune_core::bytestream::ZCursor;
                    let mut d = zune_ppm::PPMDecoder::new(ZCursor::new(&head));
                    d.decode_headers().map_err(crate::malformed)?;
                    let (w, h) = d.dimensions().ok_or_else(short)?;
                    (w as u32, h as u32)
                }
                Z::WEBP => webp_dims(&head)?,
                Z::JPEG_XL => {
                    let bytes = read_all(r)?;
                    let img = jxl_oxide::JxlImage::builder()
                        .read(std::io::Cursor::new(&bytes))
                        .map_err(malformed)?;
                    (img.width(), img.height())
                }
                _ => image_dims(r, None)?,
            }
        }
        Backend::Image => {
            let hint = ext.and_then(image::ImageFormat::from_extension);
            image_dims(r, hint)?
        }
    };
    Ok(Probe {
        width,
        height,
        format,
        hdr,
    })
}

fn io(e: std::io::Error) -> DecodeError {
    DecodeError::Other(e.to_string())
}

fn malformed(e: impl std::fmt::Display) -> DecodeError {
    DecodeError::Malformed(e.to_string())
}

fn read_all<R: Read + Seek>(r: &mut R) -> Result<Vec<u8>, DecodeError> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes).map_err(io)?;
    Ok(bytes)
}

/// Through the `image` crate's lazy header readers: `format` if given, else what it guesses.
fn image_dims<R: Read + Seek>(
    r: &mut R,
    format: Option<image::ImageFormat>,
) -> Result<(u32, u32), DecodeError> {
    let mut reader = image::ImageReader::new(BufReader::new(r))
        .with_guessed_format()
        .map_err(io)?;
    if let Some(f) = format {
        reader.set_format(f);
    }
    reader.no_limits();
    reader.into_dimensions().map_err(malformed)
}

/// The first layer with R, G and B channels, as decode reads it; its data window's size.
fn exr_dims<R: Read + Seek>(r: &mut R) -> Result<(u32, u32), DecodeError> {
    let meta =
        exr::meta::MetaData::read_from_buffered(BufReader::new(r), false).map_err(malformed)?;
    let rgb = |h: &exr::meta::header::Header| {
        ["R", "G", "B"]
            .iter()
            .all(|n| h.channels.list.iter().any(|c| c.name == **n))
    };
    let header = meta
        .headers
        .iter()
        .find(|h| rgb(h))
        .ok_or_else(|| DecodeError::Malformed("OpenEXR has no RGB layer".into()))?;
    let size = header.layer_size;
    Ok((size.width() as u32, size.height() as u32))
}

/// The Start-Of-Frame's size, walking the markers before it with seeks rather than reading the
/// EXIF thumbnails and ICC chunks in between.
fn jpeg_dims<R: Read + Seek>(r: &mut R) -> Result<(u32, u32), DecodeError> {
    let bad = || DecodeError::Malformed("JPEG has no frame header".into());
    let mut b = [0u8; 2];
    r.read_exact(&mut b).map_err(|_| bad())?;
    if b != [0xFF, 0xD8] {
        return Err(bad());
    }
    let mut byte = [0u8; 1];
    loop {
        // To the next marker: any 0xFF run, then the marker byte.
        r.read_exact(&mut byte).map_err(|_| bad())?;
        if byte[0] != 0xFF {
            continue; // fill between segments
        }
        let mut marker = 0xFF;
        while marker == 0xFF {
            r.read_exact(&mut byte).map_err(|_| bad())?;
            marker = byte[0];
        }
        // Standalone markers have no length: a stuffed 0, SOI / EOI, restarts, TEM.
        if marker == 0x00 || marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return Err(bad()); // the image ends, or its scan starts, before any frame header
        }
        r.read_exact(&mut b).map_err(|_| bad())?;
        let len = u16::from_be_bytes(b) as i64;
        if len < 2 {
            return Err(bad());
        }
        // Start-Of-Frame markers carry the size: C0..=CF except DHT (C4), JPG (C8), DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let mut sof = [0u8; 5]; // precision, height, width
            r.read_exact(&mut sof).map_err(|_| bad())?;
            let h = u16::from_be_bytes([sof[1], sof[2]]) as u32;
            let w = u16::from_be_bytes([sof[3], sof[4]]) as u32;
            return Ok((w, h));
        }
        r.seek(SeekFrom::Current(len - 2)).map_err(io)?;
    }
}

/// libwebp's own header reader: the canvas size, as decode sizes its buffer.
fn webp_dims(head: &[u8]) -> Result<(u32, u32), DecodeError> {
    use libwebp_sys as webp;
    let mut config =
        webp::WebPDecoderConfig::new().map_err(|_| malformed("libwebp ABI mismatch"))?;
    // SAFETY: `head` is a live slice for the call, and `config.input` a valid, initialized struct
    // for libwebp to fill.
    let status = unsafe { webp::WebPGetFeatures(head.as_ptr(), head.len(), &mut config.input) };
    let f = &config.input;
    match status {
        webp::VP8StatusCode::VP8_STATUS_OK => Ok((f.width as u32, f.height as u32)),
        e => Err(malformed(format!("WebP: {e:?}"))),
    }
}

/// The embedded preview's size as stored: decode with EXIF orientation off doesn't turn it.
#[cfg(feature = "raw")]
fn raw_dims<R: Read + Seek>(r: &mut R) -> Result<(u32, u32), DecodeError> {
    let bytes = read_all(r)?;
    let preview = crate::raw::find_preview(&bytes)
        .ok_or_else(|| DecodeError::Malformed("raw file has no embedded JPEG preview".into()))?;
    crate::raw::jpeg_dimensions(preview.jpeg)
        .ok_or_else(|| DecodeError::Malformed("raw preview has no frame header".into()))
}

#[cfg(not(feature = "raw"))]
fn raw_dims<R: Read + Seek>(_r: &mut R) -> Result<(u32, u32), DecodeError> {
    Err(DecodeError::Unsupported(Format::CameraRaw))
}

/// HEIF's displayed size from its boxes, as libheif computes it: the primary item's `ispe`, then
/// its transformative properties in the order `ipma` lists them, `clap` cropping (rounded) and
/// `irot` by 90 or 270 swapping. A file with no primary item takes its first visual track's
/// sample size, as decode shows that track's first frame (FD16).
mod heif {
    use std::io::{Read, Seek, SeekFrom};

    use crate::{DecodeError, HdrKind};

    /// The most of a `meta` or `moov` box read into memory. Real ones are kilobytes; a sequence's
    /// sample tables, megabytes.
    const MAX_BOX: u64 = 64 << 20;

    fn bad(what: &str) -> DecodeError {
        DecodeError::Malformed(format!("HEIF: {what}"))
    }

    /// The primary item's displayed size and HDR kind, or a sequence-only file's first track's
    /// size (whose HDR kind isn't read).
    pub(super) fn dims<R: Read + Seek>(
        r: &mut R,
        len: u64,
    ) -> Result<(u32, u32, HdrKind), DecodeError> {
        let mut meta = None;
        let mut moov = None;
        let mut pos = 0u64;
        while pos + 8 <= len {
            r.seek(SeekFrom::Start(pos)).map_err(super::io)?;
            let mut h = [0u8; 8];
            r.read_exact(&mut h).map_err(super::io)?;
            let mut size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
            let mut header = 8;
            if size == 1 {
                let mut l = [0u8; 8];
                r.read_exact(&mut l).map_err(super::io)?;
                size = u64::from_be_bytes(l);
                header = 16;
            } else if size == 0 {
                size = len - pos; // to the end of the file
            }
            if size < header || pos + size > len {
                break; // a truncated last box: what came before still counts
            }
            let kind = [h[4], h[5], h[6], h[7]];
            if (kind == *b"meta" && meta.is_none()) || (kind == *b"moov" && moov.is_none()) {
                let body_len = size - header;
                if body_len > MAX_BOX {
                    return Err(bad("metadata box is implausibly large"));
                }
                let mut body = vec![0u8; body_len as usize];
                r.read_exact(&mut body).map_err(super::io)?;
                if kind == *b"meta" {
                    meta = Some(body);
                } else {
                    moov = Some(body);
                }
            }
            pos += size;
        }
        if let Some(meta) = &meta
            && let Some(d) = primary_dims(meta)?
        {
            return Ok(d);
        }
        match &moov {
            Some(moov) => track_dims(moov).map(|(w, h)| (w, h, HdrKind::None)),
            None => Err(bad("no primary image and no track")),
        }
    }

    /// The child boxes of a box body: `(type, body)` each.
    fn boxes(mut b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
        std::iter::from_fn(move || {
            if b.len() < 8 {
                return None;
            }
            let mut size = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
            let kind = [b[4], b[5], b[6], b[7]];
            let mut header = 8;
            if size == 1 {
                size = u64::from_be_bytes(b.get(8..16)?.try_into().ok()?) as usize;
                header = 16;
            } else if size == 0 {
                size = b.len();
            }
            if size < header || size > b.len() {
                return None;
            }
            let body = &b[header..size];
            b = &b[size..];
            Some((kind, body))
        })
    }

    fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
        boxes(b).find(|(k, _)| k == kind).map(|(_, body)| body)
    }

    fn be16(b: &[u8], at: usize) -> Option<u32> {
        Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]) as u32)
    }

    fn be32(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }

    /// `None` when the file has no primary item.
    fn primary_dims(meta: &[u8]) -> Result<Option<(u32, u32, HdrKind)>, DecodeError> {
        // `meta` is a full box: version and flags, then its children.
        let meta = meta.get(4..).ok_or_else(|| bad("empty meta box"))?;
        let Some(pitm) = child(meta, b"pitm") else {
            return Ok(None);
        };
        let primary = match pitm.first() {
            Some(0) => be16(pitm, 4),
            _ => be32(pitm, 4),
        }
        .ok_or_else(|| bad("short pitm"))?;
        let iprp = child(meta, b"iprp").ok_or_else(|| bad("no iprp"))?;
        let ipco = child(iprp, b"ipco").ok_or_else(|| bad("no ipco"))?;
        let properties: Vec<([u8; 4], &[u8])> = boxes(ipco).collect();

        // Every ipma's associations for the primary item, in order: 1-based property indices.
        let mut assoc = Vec::new();
        for (kind, ipma) in boxes(iprp) {
            if kind != *b"ipma" {
                continue;
            }
            let version = *ipma.first().ok_or_else(|| bad("short ipma"))?;
            let wide = ipma.get(3).is_some_and(|f| f & 1 != 0);
            let count = be32(ipma, 4).ok_or_else(|| bad("short ipma"))?;
            let mut at = 8;
            for _ in 0..count {
                let item = if version < 1 {
                    let v = be16(ipma, at);
                    at += 2;
                    v
                } else {
                    let v = be32(ipma, at);
                    at += 4;
                    v
                }
                .ok_or_else(|| bad("short ipma"))?;
                let n = *ipma.get(at).ok_or_else(|| bad("short ipma"))? as usize;
                at += 1;
                for _ in 0..n {
                    let index = if wide {
                        let v = be16(ipma, at).map(|v| v & 0x7fff);
                        at += 2;
                        v
                    } else {
                        let v = ipma.get(at).map(|&v| (v & 0x7f) as u32);
                        at += 1;
                        v
                    }
                    .ok_or_else(|| bad("short ipma"))?;
                    if item == primary {
                        assoc.push(index as usize);
                    }
                }
            }
        }

        let prop = |i: usize| i.checked_sub(1).and_then(|i| properties.get(i));
        let ispe = assoc
            .iter()
            .filter_map(|&i| prop(i))
            .find(|(k, _)| k == b"ispe")
            .ok_or_else(|| bad("the primary item has no ispe"))?
            .1;
        let (mut w, mut h) = (
            be32(ispe, 4).ok_or_else(|| bad("short ispe"))?,
            be32(ispe, 8).ok_or_else(|| bad("short ispe"))?,
        );
        let mut hdr = HdrKind::None;
        let mut nclx_seen = false;
        for &i in &assoc {
            let Some((kind, body)) = prop(i) else {
                continue;
            };
            match kind {
                b"irot" => {
                    if body.first().is_some_and(|a| a & 1 == 1) {
                        std::mem::swap(&mut w, &mut h);
                    }
                }
                b"clap" => {
                    // Width and height as fractions; libheif rounds each to nearest.
                    let frac = |at: usize| -> Option<u32> {
                        let n = be32(body, at)? as i64;
                        let d = be32(body, at + 4)? as i32 as i64;
                        (d > 0).then(|| ((n + d / 2) / d).max(0) as u32)
                    };
                    if let (Some(cw), Some(ch)) = (frac(0), frac(8)) {
                        (w, h) = (cw.min(w), ch.min(h));
                    }
                }
                // `nclx`, then colour primaries, transfer characteristics and matrix, u16 each
                // (H.273 code points). The first nclx is the one libheif uses, as here.
                b"colr" if body.starts_with(b"nclx") && !nclx_seen => {
                    nclx_seen = true;
                    hdr = match be16(body, 6) {
                        Some(16) => HdrKind::Pq,
                        Some(18) => HdrKind::Hlg,
                        _ => HdrKind::None,
                    };
                }
                _ => {}
            }
        }
        Ok(Some((w, h, hdr)))
    }

    /// The first visual track's sample size (`moov/trak/mdia/minf/stbl/stsd`'s first entry).
    fn track_dims(moov: &[u8]) -> Result<(u32, u32), DecodeError> {
        for (kind, trak) in boxes(moov) {
            if kind != *b"trak" {
                continue;
            }
            let Some(mdia) = child(trak, b"mdia") else {
                continue;
            };
            // `hdlr`: version and flags, pre_defined, then the handler type.
            let visual = child(mdia, b"hdlr")
                .and_then(|h| h.get(8..12))
                .is_some_and(|t| t == b"pict" || t == b"vide");
            if !visual {
                continue;
            }
            let entry = child(mdia, b"minf")
                .and_then(|m| child(m, b"stbl"))
                .and_then(|s| child(s, b"stsd"))
                // A full box with an entry count, then the sample entries.
                .and_then(|s| s.get(8..))
                .and_then(|e| boxes(e).next())
                .ok_or_else(|| bad("visual track has no sample entry"))?
                .1;
            // A visual sample entry: 6 reserved, data reference index, 16 pre-defined /
            // reserved, then width and height.
            let w = be16(entry, 24).ok_or_else(|| bad("short sample entry"))?;
            let h = be16(entry, 26).ok_or_else(|| bad("short sample entry"))?;
            return Ok((w, h));
        }
        Err(bad("no visual track"))
    }
}
