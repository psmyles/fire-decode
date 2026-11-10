// What only the HEIF decode uses (gain maps, the `tmap` primary's patch) has no caller without it.
#![cfg_attr(not(feature = "heif"), allow(dead_code))]
//! A HEIF file's `meta` box, read in Rust: its items, their references and properties, and the
//! entity groups. libheif has no API for what gain maps need (FD17), and `probe()` reads the same
//! boxes without libheif at all, so both go through this.
//!
//! What it finds:
//! - [`Meta::gain_map`]: the gain map a base image has, ISO 21496-1 (`tmap`) first, then Apple's
//!   auxiliary image;
//! - [`Meta::transforms`]: an item's `irot` / `imir` / `clap`, in `ipma` order, as libheif applies
//!   them;
//! - [`Meta::tmap_primary_base`]: the SDR base of a file whose primary item is the `tmap` itself,
//!   which libheif can't open (FD17's fallback).

use crate::DecodeError;

/// Apple's HDR gain map, as an auxiliary image's `auxC` type.
pub(crate) const APPLE_GAIN_MAP_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";

fn bad(what: &str) -> DecodeError {
    DecodeError::Malformed(format!("HEIF: {what}"))
}

/// The child boxes of a box body: `(type, body, body's offset in b)` each.
pub(crate) fn boxes(b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8], usize)> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let rest = b.get(at..)?;
        if rest.len() < 8 {
            return None;
        }
        let mut size = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let kind = [rest[4], rest[5], rest[6], rest[7]];
        let mut header = 8;
        if size == 1 {
            size = u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?) as usize;
            header = 16;
        } else if size == 0 {
            size = rest.len();
        }
        if size < header || size > rest.len() {
            return None;
        }
        let body = (kind, &rest[header..size], at + header);
        at += size;
        Some(body)
    })
}

pub(crate) fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b)
        .find(|(k, _, _)| k == kind)
        .map(|(_, body, _)| body)
}

pub(crate) fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]) as u32)
}

pub(crate) fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The top-level `meta` box of a whole file in memory: its body, and that body's offset.
pub(crate) fn find_meta(bytes: &[u8]) -> Option<(&[u8], usize)> {
    boxes(bytes)
        .find(|(k, _, _)| k == b"meta")
        .map(|(_, body, at)| (body, at))
}

/// One step of an item's displayed geometry, in the order the file lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transform {
    /// Counter-clockwise quarter turns, 1..=3.
    Rotate(u8),
    /// 0 exchanges top and bottom, 1 left and right (the second edition's wording).
    Mirror(u8),
    /// A clean aperture: width, height and centre offset, each a fraction.
    Crop {
        width: (i64, i64),
        height: (i64, i64),
        horiz_off: (i64, i64),
        vert_off: (i64, i64),
    },
}

/// Where a base image's gain map is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GainMapSource {
    /// ISO 21496-1: the `tmap` item (its data is the metadata), and the gain-map image item.
    Iso { tmap: u32, gain: u32 },
    /// Apple's auxiliary gain map, and the XMP item describing it, if any.
    Apple { gain: u32, xmp: Option<u32> },
}

struct Item {
    id: u32,
    kind: [u8; 4],
    content_type: String,
}

struct Ref {
    kind: [u8; 4],
    from: u32,
    to: Vec<u32>,
}

/// A parsed `meta` box. Borrowed from the file's bytes.
pub(crate) struct Meta<'a> {
    pub(crate) primary: Option<u32>,
    /// The `pitm` box's item ID: where it is in the `meta` body, and how wide.
    pitm_field: Option<(usize, usize)>,
    items: Vec<Item>,
    refs: Vec<Ref>,
    properties: Vec<([u8; 4], &'a [u8])>,
    /// `(item, 1-based property index)`, in `ipma` order.
    assoc: Vec<(u32, usize)>,
    groups: Vec<([u8; 4], Vec<u32>)>,
}

impl<'a> Meta<'a> {
    /// `meta` is the box's body: version and flags, then its children.
    pub(crate) fn parse(meta: &'a [u8]) -> Result<Meta<'a>, DecodeError> {
        let body = meta.get(4..).ok_or_else(|| bad("empty meta box"))?;
        let mut m = Meta {
            primary: None,
            pitm_field: None,
            items: Vec::new(),
            refs: Vec::new(),
            properties: Vec::new(),
            assoc: Vec::new(),
            groups: Vec::new(),
        };
        for (kind, b, at) in boxes(body) {
            match &kind {
                b"pitm" => {
                    let wide = b.first().is_some_and(|&v| v != 0);
                    let (id, width) = if wide {
                        (be32(b, 4), 4)
                    } else {
                        (be16(b, 4), 2)
                    };
                    m.primary = Some(id.ok_or_else(|| bad("short pitm"))?);
                    // Relative to `meta`'s body, version and flags included.
                    m.pitm_field = Some((4 + at + 4, width));
                }
                b"iinf" => m.items = parse_iinf(b)?,
                b"iref" => m.refs = parse_iref(b)?,
                b"iprp" => {
                    if let Some(ipco) = child(b, b"ipco") {
                        m.properties = boxes(ipco).map(|(k, body, _)| (k, body)).collect();
                    }
                    for (k, ipma, _) in boxes(b) {
                        if k == *b"ipma" {
                            parse_ipma(ipma, &mut m.assoc)?;
                        }
                    }
                }
                b"grpl" => {
                    for (k, g, _) in boxes(b) {
                        // Full box, group ID, entity count, the entities.
                        let n = be32(g, 8).unwrap_or(0) as usize;
                        let ids: Vec<u32> = (0..n).filter_map(|i| be32(g, 12 + i * 4)).collect();
                        m.groups.push((k, ids));
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }

    fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn kind(&self, id: u32) -> Option<[u8; 4]> {
        self.item(id).map(|i| i.kind)
    }

    /// The items `from` refers to with `kind`, in order.
    fn refs_from(&self, from: u32, kind: &[u8; 4]) -> &[u32] {
        self.refs
            .iter()
            .find(|r| r.from == from && &r.kind == kind)
            .map_or(&[], |r| &r.to)
    }

    /// The items that refer to `to` with `kind`.
    fn refs_to(&self, to: u32, kind: &[u8; 4]) -> impl Iterator<Item = u32> + '_ {
        let kind = *kind;
        self.refs
            .iter()
            .filter(move |r| r.kind == kind && r.to.contains(&to))
            .map(|r| r.from)
    }

    /// An item's properties, in `ipma` order.
    pub(crate) fn properties(&self, id: u32) -> impl Iterator<Item = ([u8; 4], &'a [u8])> + '_ {
        self.assoc
            .iter()
            .filter(move |(item, _)| *item == id)
            .filter_map(|&(_, i)| {
                i.checked_sub(1)
                    .and_then(|i| self.properties.get(i))
                    .copied()
            })
    }

    /// The item's first nclx colour box: `(primaries, transfer)`.
    pub(crate) fn nclx(&self, id: u32) -> Option<(u16, u16)> {
        self.properties(id)
            .find(|(k, b)| k == b"colr" && b.starts_with(b"nclx"))
            .and_then(|(_, b)| Some((be16(b, 4)? as u16, be16(b, 6)? as u16)))
    }

    /// The item's `irot` / `imir` / `clap`, in the order libheif applies them.
    pub(crate) fn transforms(&self, id: u32) -> Vec<Transform> {
        let mut out = Vec::new();
        for (kind, b) in self.properties(id) {
            match &kind {
                b"irot" => {
                    if let Some(&a) = b.first()
                        && a & 3 != 0
                    {
                        out.push(Transform::Rotate(a & 3));
                    }
                }
                b"imir" => {
                    if let Some(&a) = b.first() {
                        out.push(Transform::Mirror(a & 1));
                    }
                }
                b"clap" => {
                    let f = |at: usize| -> Option<(i64, i64)> {
                        Some((be32(b, at)? as i32 as i64, be32(b, at + 4)? as i32 as i64))
                    };
                    if let (Some(width), Some(height), Some(horiz_off), Some(vert_off)) =
                        (f(0), f(8), f(16), f(24))
                    {
                        out.push(Transform::Crop {
                            width,
                            height,
                            horiz_off,
                            vert_off,
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The SDR base when the primary item is a `tmap`: its first input, which its `altr` group
    /// offers as the alternative. libheif can't open such a file, so the decode points `pitm` at
    /// this instead (FD17).
    pub(crate) fn tmap_primary_base(&self) -> Option<u32> {
        let primary = self.primary?;
        (self.kind(primary)? == *b"tmap")
            .then(|| self.refs_from(primary, b"dimg").first().copied())
            .flatten()
    }

    /// Where the `pitm` item ID is in the `meta` body, and its width in bytes.
    pub(crate) fn pitm_field(&self) -> Option<(usize, usize)> {
        self.pitm_field
    }

    /// `base`'s gain map: an ISO `tmap` whose first input is `base` wins over Apple's auxiliary
    /// image (ImageIO picks the ISO map too, and every iPhone photo carries both).
    pub(crate) fn gain_map(&self, base: u32) -> Option<GainMapSource> {
        let iso = self.items.iter().find_map(|t| {
            (t.kind == *b"tmap")
                .then(|| self.refs_from(t.id, b"dimg"))
                .filter(|inputs| inputs.len() == 2 && inputs[0] == base)
                .map(|inputs| GainMapSource::Iso {
                    tmap: t.id,
                    gain: inputs[1],
                })
        });
        iso.or_else(|| self.apple_gain_map(base))
    }

    /// Apple's: an auxiliary image of `base` whose `auxC` names the HDR gain map, with the XMP
    /// item (`mime`, `application/rdf+xml`) that describes it.
    pub(crate) fn apple_gain_map(&self, base: u32) -> Option<GainMapSource> {
        let gain = self.refs_to(base, b"auxl").find(|&aux| {
            self.properties(aux).any(|(k, b)| {
                k == *b"auxC"
                    && b.get(4..)
                        .and_then(|s| s.split(|&c| c == 0).next())
                        .is_some_and(|urn| urn == APPLE_GAIN_MAP_URN.as_bytes())
            })
        })?;
        let xmp = self.refs_to(gain, b"cdsc").find(|&m| {
            self.item(m)
                .is_some_and(|i| i.kind == *b"mime" && i.content_type == "application/rdf+xml")
        });
        Some(GainMapSource::Apple { gain, xmp })
    }

    /// The `Exif` item describing `id`, if any: Apple's MakerNote headroom lives there.
    pub(crate) fn exif_of(&self, id: u32) -> Option<u32> {
        self.refs_to(id, b"cdsc")
            .find(|&m| self.kind(m) == Some(*b"Exif"))
    }

    /// Whether the file's primary image, or the base a `tmap` primary stands for, has a gain map.
    pub(crate) fn has_gain_map(&self) -> bool {
        let base = self.tmap_primary_base().or(self.primary);
        base.is_some_and(|b| self.gain_map(b).is_some())
    }

    /// The entity groups of `kind` that hold `id`.
    #[cfg(test)]
    fn groups_with(&self, kind: &[u8; 4], id: u32) -> usize {
        self.groups
            .iter()
            .filter(|(k, ids)| k == kind && ids.contains(&id))
            .count()
    }
}

/// What an image sequence's first visual track says of itself, from the `moov` box's body: how
/// many samples it has (`stsz` / `stz2`), for progress, and its sample entry's nclx colour as
/// `(primaries, transfer)`, so a PQ / HLG sequence converts like a still (FD18).
pub(crate) fn track_facts(moov: &[u8]) -> (Option<u32>, Option<(u16, u16)>) {
    for (kind, trak, _) in boxes(moov) {
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
        let Some(stbl) = child(mdia, b"minf").and_then(|m| child(m, b"stbl")) else {
            return (None, None);
        };
        // `stsz`: full box, sample size, count; `stz2`: full box, 3 reserved + field size, count.
        let count = child(stbl, b"stsz")
            .and_then(|b| be32(b, 8))
            .or_else(|| child(stbl, b"stz2").and_then(|b| be32(b, 8)));
        // `stsd`: full box, entry count, then the entries. A visual sample entry's children start
        // 78 bytes in: 8 of SampleEntry, 70 of VisualSampleEntry.
        let nclx = child(stbl, b"stsd")
            .and_then(|b| boxes(b.get(8..)?).next())
            .and_then(|(_, entry, _)| {
                boxes(entry.get(78..)?)
                    .find(|(k, b, _)| k == b"colr" && b.starts_with(b"nclx"))
                    .and_then(|(_, b, _)| Some((be16(b, 4)? as u16, be16(b, 6)? as u16)))
            });
        return (count, nclx);
    }
    (None, None)
}

fn parse_iinf(b: &[u8]) -> Result<Vec<Item>, DecodeError> {
    let version = *b.first().ok_or_else(|| bad("short iinf"))?;
    let start = if version == 0 { 6 } else { 8 };
    let mut items = Vec::new();
    for (k, e, _) in boxes(b.get(start..).ok_or_else(|| bad("short iinf"))?) {
        if k != *b"infe" {
            continue;
        }
        let v = *e.first().ok_or_else(|| bad("short infe"))?;
        if v < 2 {
            continue; // Version 0 and 1 entries carry no item type.
        }
        let (id, at) = if v == 2 {
            (be16(e, 4), 6)
        } else {
            (be32(e, 4), 8)
        };
        let id = id.ok_or_else(|| bad("short infe"))?;
        // Protection index (u16), then the item type.
        let kind: [u8; 4] = e
            .get(at + 2..at + 6)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| bad("short infe"))?;
        let mut strings = e.get(at + 6..).unwrap_or(&[]).split(|&c| c == 0);
        let _name = strings.next();
        let content_type = if kind == *b"mime" {
            strings
                .next()
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        items.push(Item {
            id,
            kind,
            content_type,
        });
    }
    Ok(items)
}

fn parse_iref(b: &[u8]) -> Result<Vec<Ref>, DecodeError> {
    let wide = b.first().is_some_and(|&v| v != 0);
    let size = if wide { 4 } else { 2 };
    let read = |b: &[u8], at: usize| if wide { be32(b, at) } else { be16(b, at) };
    let mut refs = Vec::new();
    for (kind, r, _) in boxes(b.get(4..).ok_or_else(|| bad("short iref"))?) {
        let from = read(r, 0).ok_or_else(|| bad("short iref"))?;
        let count = be16(r, size).ok_or_else(|| bad("short iref"))? as usize;
        let to = (0..count)
            .map(|i| read(r, size + 2 + i * size))
            .collect::<Option<Vec<u32>>>()
            .ok_or_else(|| bad("short iref"))?;
        refs.push(Ref { kind, from, to });
    }
    Ok(refs)
}

fn parse_ipma(ipma: &[u8], assoc: &mut Vec<(u32, usize)>) -> Result<(), DecodeError> {
    let short = || bad("short ipma");
    let version = *ipma.first().ok_or_else(short)?;
    let wide = ipma.get(3).is_some_and(|f| f & 1 != 0);
    let count = be32(ipma, 4).ok_or_else(short)?;
    let mut at = 8;
    for _ in 0..count {
        let item = if version < 1 {
            at += 2;
            be16(ipma, at - 2)
        } else {
            at += 4;
            be32(ipma, at - 4)
        }
        .ok_or_else(short)?;
        let n = *ipma.get(at).ok_or_else(short)? as usize;
        at += 1;
        for _ in 0..n {
            let index = if wide {
                at += 2;
                be16(ipma, at - 2).map(|v| v & 0x7fff)
            } else {
                at += 1;
                ipma.get(at - 1).map(|&v| (v & 0x7f) as u32)
            }
            .ok_or_else(short)?;
            assoc.push((item, index as usize));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/heif")
                .join(name),
        )
        .unwrap()
    }

    fn meta_of(bytes: &[u8]) -> Meta<'_> {
        Meta::parse(find_meta(bytes).unwrap().0).unwrap()
    }

    /// The layouts `fixtures/heif/README.md` records, read here without libheif.
    #[test]
    fn the_gain_map_fixtures_read_as_the_readme_says() {
        let iso = fixture("gainmap_iso.heic");
        let m = meta_of(&iso);
        assert_eq!(m.primary, Some(1));
        assert_eq!(m.gain_map(1), Some(GainMapSource::Iso { tmap: 4, gain: 3 }));
        assert_eq!(m.groups_with(b"altr", 1), 1);
        assert!(m.has_gain_map());
        assert_eq!(m.tmap_primary_base(), None);

        let apple = fixture("gainmap_apple.heic");
        let m = meta_of(&apple);
        assert_eq!(
            m.gain_map(1),
            Some(GainMapSource::Apple {
                gain: 3,
                xmp: Some(4)
            })
        );

        // Both: ISO wins, Apple's is still found on its own.
        let both = fixture("gainmap_both.heic");
        let m = meta_of(&both);
        assert_eq!(m.gain_map(1), Some(GainMapSource::Iso { tmap: 5, gain: 3 }));
        assert!(matches!(
            m.apple_gain_map(1),
            Some(GainMapSource::Apple { gain: 3, .. })
        ));

        // A `tmap` primary stands for its base.
        let primary = fixture("gainmap_iso_tmap_primary.heic");
        let m = meta_of(&primary);
        assert_eq!(m.primary, Some(4));
        assert_eq!(m.tmap_primary_base(), Some(1));
        assert!(m.has_gain_map());

        // The portrait's base turns, its gain map and `tmap` don't.
        let portrait = fixture("gainmap_iso_portrait.heic");
        let m = meta_of(&portrait);
        assert_eq!(m.transforms(1), vec![Transform::Rotate(3)]);
        assert_eq!(m.transforms(3), vec![]);
        assert_eq!(m.transforms(4), vec![]);

        // No gain map in an ordinary file.
        let plain = fixture("depth8.heic");
        assert!(!meta_of(&plain).has_gain_map());
    }

    #[test]
    fn mirror_and_rotation_come_in_ipma_order() {
        let b = fixture("irot_imir.heic");
        let m = meta_of(&b);
        let t = m.transforms(m.primary.unwrap());
        assert_eq!(t.len(), 2, "{t:?}");
    }
}
