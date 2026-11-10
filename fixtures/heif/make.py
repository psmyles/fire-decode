#!/usr/bin/env python3
"""Builds the HEIF / HEIC / AVIF test files in this folder (fire-decode plan, migration step 0).

    python3 -I fixtures/heif/make.py           rebuild every file, check each, write expected.json
    python3 -I fixtures/heif/make.py --check   check the committed files, rebuilding nothing

Needs libheif's `heif-enc`, `heif-info` and `heif-dec` with the x265 and aom encoders (Homebrew's
`libheif`), ImageMagick's `magick` (only to read `heif-dec`'s PNGs back), numpy and Pillow.

Every source picture is generated here, so every file may be redistributed. Each file is checked two
ways after it is written:
- libheif decodes it back (`heif-dec`), and the pixels are compared with what the source says they
  must be: corner markers for orientation, code values for PQ / HLG, colours for sequences;
- what the file must contain (sizes, `irot` / `imir`, grid layout, nclx, box order) is read from its
  boxes by this script, not from libheif, so a libheif mistake can't hide behind its own report.

`expected.json` records, per file, what a decoder must return: the displayed size, bit depth, nclx,
transforms, patches with their code values and the light they stand for, frames, or the error. The
tests in migration steps 2b and 6 read it.
"""

import json
import math
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
TOOLS = ("heif-enc", "heif-info", "heif-dec", "magick")

# ---------------------------------------------------------------------------------------------
# Running tools


def run(*args, cwd=None, check=True):
    result = subprocess.run([str(a) for a in args], cwd=cwd, capture_output=True, text=True)
    if check and result.returncode != 0:
        raise RuntimeError(f"{' '.join(map(str, args))} failed:\n{result.stdout}{result.stderr}")
    return result


def heif_enc(src, out, *flags, cwd=None):
    run("heif-enc", *flags, "-o", out, src, cwd=cwd)


def decode(path, tmp, sequence=False):
    """libheif's decode of `path` as float RGBA in 0..1 plus the 16-bit values: a list of frames."""
    out = Path(tmp) / (Path(path).stem + "-dec.png")
    # The raw timeline: following the edit list, an infinitely repeating file never ends.
    flags = ["-S", "--ignore-editlist"] if sequence else []
    run("heif-dec", *flags, path, out)
    if sequence:
        files = sorted(out.parent.glob(out.stem + "*.png"),
                       key=lambda f: int("".join(ch for ch in f.stem[len(out.stem):] if ch.isdigit()) or 0))
    else:
        files = [out]
    frames = []
    for f in files:
        w, h = map(int, run("magick", "identify", "-format", "%w %h", f).stdout.split())
        raw = subprocess.run(["magick", str(f), "-depth", "16", "-endian", "MSB", "rgba:-"],
                             capture_output=True, check=True).stdout
        v16 = np.frombuffer(raw, ">u2").reshape(h, w, 4).astype(np.uint32)
        frames.append(v16)
        f.unlink()
    return frames


# ---------------------------------------------------------------------------------------------
# Writing sources


def png16(path, arr):
    """A 16-bit RGB / RGBA PNG from a uint16 array (Pillow can't write 16-bit colour)."""
    h, w, c = arr.shape
    rows = arr.astype(">u2").reshape(h, w * c)
    raw = b"".join(b"\0" + rows[i].tobytes() for i in range(h))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    colour_type = {3: 2, 4: 6}[c]
    Path(path).write_bytes(b"\x89PNG\r\n\x1a\n"
                           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 16, colour_type, 0, 0, 0))
                           + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def png8(path, img):
    Image.fromarray(np.clip(np.rint(np.asarray(img) * 255), 0, 255).astype(np.uint8)).save(path)


def to16(img):
    return np.clip(np.rint(img * 65535), 0, 65535).astype(np.uint16)


CORNERS = {"tl": (1.0, 0.0, 0.0), "tr": (0.0, 1.0, 0.0), "bl": (0.0, 0.0, 1.0), "br": (1.0, 1.0, 1.0)}


def marked(h, w):
    """A ramp with a coloured block in each corner: red top-left, green top-right, blue bottom-left,
    white bottom-right. Where the blocks land after decoding shows the orientation."""
    y, x = np.mgrid[0:h, 0:w]
    img = np.empty((h, w, 3))
    img[..., 0] = 0.15 + 0.7 * x / max(w - 1, 1)
    img[..., 1] = 0.15 + 0.7 * y / max(h - 1, 1)
    img[..., 2] = 0.5
    b = block_size(h, w)
    img[:b, :b] = CORNERS["tl"]
    img[:b, -b:] = CORNERS["tr"]
    img[-b:, :b] = CORNERS["bl"]
    img[-b:, -b:] = CORNERS["br"]
    return img


def block_size(h, w):
    return max(16, min(h, w) // 8)


def check_corners(name, rgba16, h, w, tolerance=0.2):
    """The decoded image is h x w and each corner block holds its colour."""
    got_h, got_w = rgba16.shape[:2]
    assert (got_w, got_h) == (w, h), f"{name}: decoded {got_w}x{got_h}, expected {w}x{h}"
    img = rgba16[..., :3] / 65535.0
    b = block_size(h, w)
    centres = {"tl": (b // 2, b // 2), "tr": (b // 2, w - 1 - b // 2),
               "bl": (h - 1 - b // 2, b // 2), "br": (h - 1 - b // 2, w - 1 - b // 2)}
    for corner, (y, x) in centres.items():
        got = img[y, x]
        nearest = min(CORNERS, key=lambda c: np.linalg.norm(got - CORNERS[c]))
        assert nearest == corner and np.abs(got - CORNERS[corner]).max() < tolerance, \
            f"{name}: {corner} corner is {np.round(got, 3)}, expected {CORNERS[corner]}"


def corners_entry(h, w):
    return {"block": block_size(h, w), "colours": {k: list(v) for k, v in CORNERS.items()}}


# ---------------------------------------------------------------------------------------------
# Colour: SMPTE ST 2084 (PQ), BT.2100 HLG, sRGB, and conversion between primaries


PQ_M1, PQ_M2 = 2610 / 16384, 2523 / 4096 * 128
PQ_C1, PQ_C2, PQ_C3 = 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32


def pq_signal(nits):
    y = (nits / 10000) ** PQ_M1
    return ((PQ_C1 + PQ_C2 * y) / (1 + PQ_C3 * y)) ** PQ_M2


def pq_nits(signal):
    p = signal ** (1 / PQ_M2)
    return 10000 * (max(p - PQ_C1, 0) / (PQ_C2 - PQ_C3 * p)) ** (1 / PQ_M1)


HLG_A = 0.17883277
HLG_B = 1 - 4 * HLG_A
HLG_C = 0.5 - HLG_A * math.log(4 * HLG_A)
HLG_PEAK, HLG_GAMMA = 1000.0, 1.2  # FD17: a 1000 cd/m2 nominal display, system gamma 1.2


def hlg_scene(signal):
    """BT.2100 inverse OETF: scene light 0..1 from the signal."""
    return signal * signal / 3 if signal <= 0.5 else (math.exp((signal - HLG_C) / HLG_A) + HLG_B) / 12


def hlg_display_nits(rgb_signal):
    """BT.2100 HLG display light (cd/m2) for a BT.2020 signal, through the OOTF."""
    scene = np.array([hlg_scene(s) for s in rgb_signal])
    ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2]
    return HLG_PEAK * (ys ** (HLG_GAMMA - 1) if ys > 0 else 0.0) * scene


PRIMARIES = {
    "bt709": ((0.640, 0.330), (0.300, 0.600), (0.150, 0.060)),
    "bt2020": ((0.708, 0.292), (0.170, 0.797), (0.131, 0.046)),
    "p3": ((0.680, 0.320), (0.265, 0.690), (0.150, 0.060)),
}
D65 = (0.3127, 0.3290)


def to_xyz(primaries):
    m = np.array([[x / y, 1.0, (1 - x - y) / y] for x, y in PRIMARIES[primaries]]).T
    white = np.array([D65[0] / D65[1], 1.0, (1 - D65[0] - D65[1]) / D65[1]])
    return m * np.linalg.solve(m, white)


def convert(src, dst):
    return np.linalg.inv(to_xyz(dst)) @ to_xyz(src)


def srgb_decode(v):
    v = np.asarray(v, float)
    return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4)


def srgb_encode(v):
    v = np.clip(np.asarray(v, float), 0, 1)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * v ** (1 / 2.4) - 0.055)


SDR_WHITE = 203.0  # FD17: BT.2408 reference white = 1.0 in a Linear buffer

# ---------------------------------------------------------------------------------------------
# ISOBMFF boxes: just enough to read properties and to move boxes around


class Box:
    def __init__(self, kind, off, size, hdr):
        self.kind, self.off, self.size, self.hdr = kind, off, size, hdr

    @property
    def body(self):
        return self.off + self.hdr

    @property
    def end(self):
        return self.off + self.size


CONTAINERS = {"moov": 0, "trak": 0, "mdia": 0, "minf": 0, "stbl": 0, "dinf": 0, "iprp": 0,
              "ipco": 0, "meta": 4, "iref": 4}


def boxes(b, start=0, end=None):
    end = len(b) if end is None else end
    out, off = [], start
    while off + 8 <= end:
        size, kind = struct.unpack_from(">I4s", b, off)
        hdr = 8
        if size == 1:
            size, hdr = struct.unpack_from(">Q", b, off + 8)[0], 16
        elif size == 0:
            size = end - off
        if size < hdr or off + size > end:
            raise ValueError(f"bad box at {off}")
        out.append(Box(kind.decode("latin1"), off, size, hdr))
        off += size
    return out


def children(b, box):
    return boxes(b, box.body + CONTAINERS[box.kind], box.end)


def find(b, *path):
    """The first box at `path` (e.g. "meta", "iprp", "ipco"), or None."""
    level = boxes(b)
    box = None
    for kind in path:
        box = next((x for x in level if x.kind == kind), None)
        if box is None:
            return None
        if kind in CONTAINERS:
            level = children(b, box)
    return box


def find_all(b, kind, start=0, end=None):
    """Every box of `kind`, searched through containers."""
    found = []
    for box in boxes(b, start, end):
        if box.kind == kind:
            found.append(box)
        if box.kind in CONTAINERS:
            found += find_all(b, kind, box.body + CONTAINERS[box.kind], box.end)
    return found


def uint(b, off, n):
    return int.from_bytes(b[off:off + n], "big") if n else 0


def put_uint(b, off, n, value):
    b[off:off + n] = value.to_bytes(n, "big")


def iloc_fields(b):
    """(position, size, kind) of every offset field in iloc that holds a file offset: base offsets
    and extent offsets of items stored in the file (construction method 0)."""
    iloc = find(b, "meta", "iloc")
    p = iloc.body
    version = b[p]
    p += 4
    offset_size, length_size = b[p] >> 4, b[p] & 15
    base_size, index_size = b[p + 1] >> 4, (b[p + 1] & 15) if version in (1, 2) else 0
    p += 2
    count = uint(b, p, 4 if version == 2 else 2)
    p += 4 if version == 2 else 2
    fields = []
    for _ in range(count):
        p += 4 if version == 2 else 2
        method = 0
        if version in (1, 2):
            method = uint(b, p, 2) & 15
            p += 2
        p += 2  # data_reference_index
        base_pos = p
        base = uint(b, p, base_size)
        p += base_size
        extents = uint(b, p, 2)
        p += 2
        use_base = method == 0 and base_size and (base or not offset_size)
        if use_base:
            fields.append((base_pos, base_size, "base"))
        for _ in range(extents):
            p += index_size
            if method == 0 and not use_base and offset_size:
                fields.append((p, offset_size, "extent"))
            p += offset_size
            fields.append((p, length_size, "length"))
            p += length_size
    return fields


def chunk_offset_fields(b):
    fields = []
    for kind, n in (("stco", 4), ("co64", 8)):
        for box in find_all(b, kind):
            count = uint(b, box.body + 4, 4)
            fields += [(box.body + 8 + i * n, n) for i in range(count)]
    return fields


def relayout(b, parts):
    """A new file from `parts`: (start, end) ranges of `b` to copy, or bytes to insert. File offsets
    in iloc and stco / co64 that pointed into a copied range are moved with it."""
    out, moves = bytearray(), []
    for part in parts:
        if isinstance(part, tuple):
            moves.append((part[0], part[1], len(out)))
            out += b[part[0]:part[1]]
        else:
            out += part

    def moved(off):
        for start, end, new in moves:
            if start <= off < end:
                return new + off - start
        raise ValueError(f"offset {off} points into a part that was dropped")

    if find(out, "meta") and find(out, "meta", "iloc"):
        for pos, n, kind in iloc_fields(out):
            if kind != "length":
                put_uint(out, pos, n, moved(uint(out, pos, n)))
    for pos, n in chunk_offset_fields(out):
        put_uint(out, pos, n, moved(uint(out, pos, n)))
    return bytes(out)


def resize_box(b, box, new_body):
    """`b` with `box`'s body replaced, its size field and every enclosing box's size updated."""
    out = bytearray(b[:box.body] + new_body + b[box.end:])
    delta = len(new_body) - (box.end - box.body)
    for outer in enclosing(b, box):
        assert outer.hdr == 8
        put_uint(out, outer.off, 4, outer.size + delta)
    return bytes(out)


def enclosing(b, target, start=0, end=None):
    """`target` and every box that contains it."""
    for box in boxes(b, start, end):
        if box.off == target.off and box.kind == target.kind:
            return [box]
        if box.kind in CONTAINERS and box.off < target.off < box.end:
            return enclosing(b, target, box.body + CONTAINERS[box.kind], box.end) + [box]
    raise ValueError("box not found")


def primary_properties(b):
    """The primary item's properties, in ipma order, as boxes."""
    pitm = find(b, "meta", "pitm")
    item = uint(b, pitm.body + 4, 2 if b[pitm.body] == 0 else 4)
    props = children(b, find(b, "meta", "iprp", "ipco"))
    ipma = find(b, "meta", "iprp", "ipma")
    version, flags = b[ipma.body], uint(b, ipma.body + 1, 3)
    p = ipma.body + 4
    for _ in range(uint(b, p, 4)):
        p += 4
        item_id = uint(b, p, 2 if version < 1 else 4)
        p += 2 if version < 1 else 4
        count = b[p]
        p += 1
        indices = []
        for _ in range(count):
            if flags & 1:
                indices.append(uint(b, p, 2) & 0x7FFF)
                p += 2
            else:
                indices.append(b[p] & 0x7F)
                p += 1
        if item_id == item:
            return [props[i - 1] for i in indices if i]
    raise ValueError("primary item has no properties")


def transforms(b):
    """The primary item's irot / imir, in the order they apply (ISO/IEC 23008-12)."""
    out = []
    for prop in primary_properties(b):
        if prop.kind == "irot":
            out.append(("irot", (b[prop.body] & 3) * 90))
        elif prop.kind == "imir":
            out.append(("imir", b[prop.body] & 1))
        elif prop.kind == "clap":
            out.append(("clap", None))
    return out


def apply_transforms(img, ops):
    """What a decoder shows for coded pixels `img`, applying the properties in ipma order. irot
    turns anti-clockwise. imir axis 0 exchanges the top and bottom, axis 1 the left and right: the
    wording of ISO/IEC 23008-12's second edition, and libavif's. The first edition's "vertical
    axis" for 0 was read both ways."""
    for kind, value in ops:
        if kind == "irot":
            img = np.rot90(img, k=value // 90)
        elif kind == "imir":
            img = np.flipud(img) if value == 0 else np.fliplr(img)
    return img


def ispe_sizes(b):
    return [(uint(b, box.body + 4, 4), uint(b, box.body + 8, 4)) for box in find_all(b, "ispe")]


def nclx(b):
    for prop in primary_properties(b):
        if prop.kind == "colr" and b[prop.body:prop.body + 4] == b"nclx":
            p = prop.body + 4
            return {"colour_primaries": uint(b, p, 2), "transfer_characteristics": uint(b, p + 2, 2),
                    "matrix_coefficients": uint(b, p + 4, 2), "full_range": bool(b[p + 6] >> 7)}
    return None


def has_icc(b):
    return any(p.kind == "colr" and b[p.body:p.body + 4] in (b"prof", b"rICC")
               for p in primary_properties(b))


def top_level(b):
    return [x.kind for x in boxes(b)]


def items(b):
    """{item id: item type} from iinf."""
    iinf = find(b, "meta", "iinf")
    p = iinf.body + 4 + (2 if b[iinf.body] == 0 else 4)
    out = {}
    for box in boxes(b, p, iinf.end):
        q = box.body
        version = b[q]
        q += 4
        n = 2 if version < 3 else 4
        item_id = uint(b, q, n)
        out[item_id] = b[q + n + 2:q + n + 6].decode("latin1")
    return out


def item_properties(b):
    """{item id: [property boxes]} from ipma."""
    props = children(b, find(b, "meta", "iprp", "ipco"))
    ipma = find(b, "meta", "iprp", "ipma")
    version, flags = b[ipma.body], uint(b, ipma.body + 1, 3)
    p = ipma.body + 4
    count = uint(b, p, 4)
    p += 4
    out = {}
    for _ in range(count):
        item_id = uint(b, p, 2 if version < 1 else 4)
        p += 2 if version < 1 else 4
        n = b[p]
        p += 1
        indices = []
        for _ in range(n):
            if flags & 1:
                indices.append(uint(b, p, 2) & 0x7FFF)
                p += 2
            else:
                indices.append(b[p] & 0x7F)
                p += 1
        out[item_id] = [props[i - 1] for i in indices if i]
    return out


def drop_property(b, item_id, kind):
    """`b` with `item_id`'s association to its `kind` property set to index 0, "no property",
    which keeps every size and offset."""
    props = children(b, find(b, "meta", "iprp", "ipco"))
    ipma = find(b, "meta", "iprp", "ipma")
    version, flags = b[ipma.body], uint(b, ipma.body + 1, 3)
    out = bytearray(b)
    p = ipma.body + 4
    count = uint(b, p, 4)
    p += 4
    for _ in range(count):
        this = uint(b, p, 2 if version < 1 else 4)
        p += 2 if version < 1 else 4
        n = b[p]
        p += 1
        for _ in range(n):
            size = 2 if flags & 1 else 1
            index = uint(b, p, size) & (0x7FFF if size == 2 else 0x7F)
            if this == item_id and index and props[index - 1].kind == kind:
                put_uint(out, p, size, 0)
                return bytes(out)
            p += size
    raise ValueError(f"item {item_id} has no {kind}")


def aux_types(b):
    """{item id: auxC URN} for auxiliary images."""
    out = {}
    for item_id, props in item_properties(b).items():
        for prop in props:
            if prop.kind == "auxC":
                urn = b[prop.body + 4:prop.end]
                out[item_id] = urn[:urn.index(b"\0")].decode() if b"\0" in urn else urn.decode()
    return out


def rotations(b):
    """{item id: irot angle anti-clockwise} for the items that have one."""
    return {item_id: (b[prop.body] & 3) * 90 for item_id, props in item_properties(b).items()
            for prop in props if prop.kind == "irot"}


def heif_info(path):
    return run("heif-info", path).stdout


# ---------------------------------------------------------------------------------------------
# The files


EXPECTED = {}


def entry(name, **fields):
    EXPECTED[name] = fields


def build_depths(tmp, build):
    """8 / 10 / 12-bit HEIC and AVIF: a ramp 1024 wide, so 10 and 12 bits have more than 256
    levels along a row to keep."""
    h, w = 192, 1024
    img = marked(h, w)
    png8(tmp / "d8.png", img)
    png16(tmp / "d16.png", to16(img))
    for ext, codec in (("heic", []), ("avif", ["-A"])):
        for depth in (8, 10, 12):
            name = f"depth{depth}.{ext}"
            if build:
                src = tmp / ("d8.png" if depth == 8 else "d16.png")
                heif_enc(src, HERE / name, *codec, "-q", "85", *([] if depth == 8 else ["-b", depth]))
            info = heif_info(HERE / name)
            assert f"bit depth: {depth}" in info, f"{name}: not {depth}-bit"
            (frame,) = decode(HERE / name, tmp)
            check_corners(name, frame, h, w)
            levels = len(np.unique(frame[h // 2, block_size(h, w):-block_size(h, w), 0]))
            assert (levels > 300) if depth > 8 else (levels <= 256), f"{name}: {levels} levels"
            entry(name, kind="still", width=w, height=h, bit_depth=depth, corners=corners_entry(h, w),
                  expect="decodes", notes="A ramp along the width: more than 256 levels unless 8-bit.")


def build_grid(tmp, build):
    """An iPhone-style grid: 512 px tiles, partial tiles on the right and bottom, a thumbnail."""
    h, w = 1500, 2000
    name = "grid_iphone.heic"
    if build:
        png8(tmp / "grid.png", marked(h, w))
        heif_enc(tmp / "grid.png", HERE / name, "-q", "70", "--cut-tiles", "512", "-t", "320")
    b = (HERE / name).read_bytes()
    info = heif_info(HERE / name)
    assert "tiles: 4x3, tile size: 512x512" in info and "thumbnail: 320x240" in info, info
    assert (w, h) in ispe_sizes(b) and (512, 512) in ispe_sizes(b)
    (frame,) = decode(HERE / name, tmp)
    check_corners(name, frame, h, w)
    entry(name, kind="still", width=w, height=h, bit_depth=8, corners=corners_entry(h, w),
          grid={"columns": 4, "rows": 3, "tile": 512}, thumbnail=[320, 240], expect="decodes",
          notes="4x3 tiles cover 2048x1536; the grid crops them to 2000x1500, as iPhone grids do.")


def build_oriented(tmp, build):
    """irot, imir and both. The coded pixels are chosen so that, transformed as the boxes say, the
    picture is upright (red top-left)."""
    h, w = 256, 192  # portrait, as shown
    upright = marked(h, w)
    cases = [("portrait_irot.heic", ["--rotate-cw", "90"]),
             ("mirror_imir.heic", ["--flip-h"]),
             ("irot_imir.heic", ["--rotate-cw", "90", "--flip-h"])]
    for name, flags in cases:
        if build:
            png8(tmp / "probe.png", upright)
            heif_enc(tmp / "probe.png", tmp / "probe.heic", "-q", "90", *flags)
            ops = transforms((tmp / "probe.heic").read_bytes())
            variants = [np.rot90(upright, k) for k in range(4)]
            variants += [np.fliplr(v) for v in variants]
            coded = next(v for v in variants if np.array_equal(apply_transforms(v, ops), upright))
            png8(tmp / "coded.png", coded)
            heif_enc(tmp / "coded.png", HERE / name, "-q", "90", *flags)
        b = (HERE / name).read_bytes()
        ops = transforms(b)
        wanted = {"--rotate-cw": "irot", "--flip-h": "imir"}
        assert sorted(k for k, _ in ops) == sorted(wanted[f] for f in flags if f in wanted), ops
        coded_w, coded_h = ispe_sizes(b)[0]
        (frame,) = decode(HERE / name, tmp)
        check_corners(name, frame, h, w)
        entry(name, kind="still", width=w, height=h, coded=[coded_w, coded_h], bit_depth=8,
              transforms=[{"kind": k, "value": v} for k, v in ops], corners=corners_entry(h, w),
              expect="decodes", notes="Shown upright, red top-left, only once the transforms apply.")


def hdr_patches(kind):
    """Two rows of nine 32 px patches: neutrals, then colours. PQ in cd/m2, HLG as signal."""
    if kind == "pq":
        neutral = [0, 0.1, 1, 10, 100, 203, 1000, 4000, 10000]
        colours = [(203, 0, 0), (0, 203, 0), (0, 0, 203), (203, 203, 0), (1000, 0, 0), (0, 1000, 0),
                   (0, 0, 1000), (50, 100, 200), (203, 203, 203)]
        signal = lambda rgb: [pq_signal(v) for v in rgb]
    else:
        neutral = [0, 0.05, 0.1, 0.25, 0.5, 0.6, 0.75, 0.9, 1.0]
        colours = [(0.75, 0, 0), (0, 0.75, 0), (0, 0, 0.75), (0.75, 0.75, 0), (1, 0, 0), (0, 1, 0),
                   (0, 0, 1), (0.3, 0.5, 0.7), (0.75, 0.75, 0.75)]
        signal = lambda rgb: list(rgb)
    rows = [[(v, v, v) for v in neutral], colours]
    patches = []
    for r, row in enumerate(rows):
        for c, value in enumerate(row):
            code = [round(s * 1023) for s in signal(value)]
            if kind == "pq":
                nits = np.array([pq_nits(v / 1023) for v in code])
            else:
                nits = hlg_display_nits([v / 1023 for v in code])
            linear = convert("bt2020", "bt709") @ (nits / SDR_WHITE)
            patches.append({"x": c * 32, "y": r * 32, "size": 32, "code10": code,
                            "nits_bt2020": [round(v, 4) for v in nits],
                            "linear_bt709": [round(v, 6) for v in linear]})
    return patches


def build_hdr(tmp, build):
    """PQ and HLG, BT.2020, 10-bit, with patches of known light: lossless (exact code values) in
    both codecs, and one lossy YCbCr (matrix 9) AVIF as such files usually come."""
    h, w = 64, 288
    for kind, transfer in (("pq", 16), ("hlg", 18)):
        patches = hdr_patches(kind)
        arr = np.zeros((h, w, 3), np.uint16)
        for p in patches:
            arr[p["y"]:p["y"] + 32, p["x"]:p["x"] + 32] = np.array(p["code10"]) << 6
        png16(tmp / f"{kind}.png", arr)
        cases = [(f"{kind}_lossless.heic", ["-L"], 0, True), (f"{kind}_lossless.avif", ["-A", "-L"], 0, True)]
        if kind == "pq":
            cases.append(("pq_ycbcr.avif", ["-A", "-q", "92", "--matrix_coefficients", "9"], 9, False))
        for name, flags, matrix, exact in cases:
            if build:
                heif_enc(tmp / f"{kind}.png", HERE / name, *flags, "-b", "10", "--colour_primaries", "9",
                         "--transfer_characteristic", transfer)
            b = (HERE / name).read_bytes()
            got = nclx(b)
            assert got and (got["colour_primaries"], got["transfer_characteristics"],
                            got["matrix_coefficients"]) == (9, transfer, matrix), f"{name}: nclx {got}"
            (frame,) = decode(HERE / name, tmp)
            for p in patches:
                inner = frame[p["y"] + 4:p["y"] + 28, p["x"] + 4:p["x"] + 28, :3] >> 6
                err = np.abs(inner.astype(int) - np.array(p["code10"])).max()
                assert err == 0 if exact else err <= 6, f"{name}: patch at {p['x']},{p['y']} off by {err}"
            entry(name, kind="still", width=w, height=h, bit_depth=10, nclx=got,
                  hdr="pq" if kind == "pq" else "hlg", patches=patches,
                  expect="decodes", notes=("Code values exact." if exact else "Lossy: code values within 6.")
                  + " linear_bt709 is FD17's Linear output: cd/m2 / 203, converted to Rec.709 primaries.")


def build_p3(tmp, build):
    """Display P3 declared only by nclx (primaries 12), no ICC profile: common in Android AVIF."""
    colours = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255), (200, 120, 60), (60, 160, 90)]
    h, w = 32, 32 * len(colours)
    img = np.zeros((h, w, 3))
    for i, c in enumerate(colours):
        img[:, i * 32:(i + 1) * 32] = np.array(c) / 255
    png8(tmp / "p3.png", img)
    to_srgb = convert("p3", "bt709")
    patches = []
    for i, c in enumerate(colours):
        linear = to_srgb @ srgb_decode(np.array(c) / 255)
        patches.append({"x": i * 32, "y": 0, "size": 32, "code8": list(c),
                        "linear_bt709": [round(v, 6) for v in linear],
                        "srgb8_clipped": [int(round(v * 255)) for v in srgb_encode(linear)]})
    for name, codec in (("p3_nclx.heic", []), ("p3_nclx.avif", ["-A"])):
        if build:
            heif_enc(tmp / "p3.png", HERE / name, *codec, "-q", "92", "--colour_primaries", "12",
                     "--transfer_characteristic", "13", "--matrix_coefficients", "6")
        b = (HERE / name).read_bytes()
        got = nclx(b)
        assert got and got["colour_primaries"] == 12 and not has_icc(b), f"{name}: {got}"
        (frame,) = decode(HERE / name, tmp)
        for p in patches:
            inner = frame[8:24, p["x"] + 8:p["x"] + 24, :3] / 257
            err = np.abs(inner - np.array(p["code8"])).max()
            assert err <= 8, f"{name}: patch {p['code8']} off by {err:.1f}"
        entry(name, kind="still", width=w, height=h, bit_depth=8, nclx=got, icc=False, patches=patches,
              expect="decodes", notes="viewer() converts to sRGB (srgb8_clipped); the other setups "
              "return code8 as stored (FD5).")


def build_premultiplied(tmp, build):
    """Premultiplied alpha, lossless: straight (200, 100, 50) at four alphas."""
    alphas = [255, 192, 128, 64]
    straight = np.array([200, 100, 50])
    h, w = 32, 32 * len(alphas)
    arr = np.zeros((h, w, 4), np.uint8)
    for i, a in enumerate(alphas):
        arr[:, i * 32:(i + 1) * 32, :3] = np.rint(straight * a / 255)
        arr[:, i * 32:(i + 1) * 32, 3] = a
    Image.fromarray(arr, "RGBA").save(tmp / "pm.png")
    name = "premultiplied_alpha.heic"
    if build:
        heif_enc(tmp / "pm.png", HERE / name, "-L", "--premultiplied-alpha")
    assert "premultiplied" in heif_info(HERE / name)
    # libheif hands back the stored, premultiplied values; un-premultiplying is the caller's job
    # (fire-decode does it today, and FD16 keeps it).
    (frame,) = decode(HERE / name, tmp)
    for i, a in enumerate(alphas):
        px = frame[16, i * 32 + 16] / 257
        stored = np.rint(straight * a / 255)
        assert abs(px[3] - a) <= 1, f"{name}: alpha {px[3]:.0f}, expected {a}"
        assert np.abs(px[:3] - stored).max() <= 1, f"{name}: colour {np.round(px[:3])} at alpha {a}"
    entry(name, kind="still", width=w, height=h, bit_depth=8, alpha="premultiplied",
          patches=[{"x": i * 32, "y": 0, "size": 32, "alpha": a, "stored8": np.rint(straight * a / 255)
                    .astype(int).tolist(), "straight8": straight.tolist()} for i, a in enumerate(alphas)],
          expect="decodes", notes="libheif returns stored8; un-premultiplied, every patch reads "
          "straight8 (200, 100, 50) within rounding.")


SEQ_COLOURS = [(230, 25, 75), (60, 180, 75), (255, 225, 25), (0, 130, 200), (245, 130, 48),
               (145, 30, 180), (70, 240, 240), (240, 50, 230)]


def encode_sequence(tmp, out, frames, flags):
    """`frames` may be a generator: each frame is written out before the next is made."""
    folder = Path(tempfile.mkdtemp(dir=tmp))
    for i, frame in enumerate(frames):
        png8(folder / f"f-{i:02d}.png", frame)
        del frame
    # heif-enc finds the other frames from the first one's name, and only relative to its cwd.
    heif_enc("f-00.png", out, "-S", *flags, cwd=folder)
    shutil.rmtree(folder)


def sample_count(b):
    stsz = find_all(b, "stsz")[0]
    return uint(b, stsz.body + 8, 4)


def check_frames(name, tmp, colours, size):
    frames = decode(HERE / name, tmp, sequence=True)
    assert len(frames) == len(colours), f"{name}: {len(frames)} frames decoded, expected {len(colours)}"
    for i, (frame, c) in enumerate(zip(frames, colours)):
        assert frame.shape[:2] == (size, size)
        err = np.abs(frame[size // 2, size // 2, :3] / 257 - np.array(c)).max()
        assert err <= 12, f"{name}: frame {i} off by {err:.0f}"


def build_sequences(tmp, build):
    size = 64
    flat = [np.full((size, size, 3), np.array(c) / 255) for c in SEQ_COLOURS]

    name = "seq_infinite.avif"
    if build:
        encode_sequence(tmp, HERE / name, flat, ["-A", "-q", "80", "--fps", "10", "--repetitions", "infinite"])
    b = (HERE / name).read_bytes()
    assert "repetitions: infinite" in heif_info(HERE / name) and find(b, "meta", "pitm")
    assert sample_count(b) == len(flat)
    check_frames(name, tmp, SEQ_COLOURS, size)
    entry(name, kind="sequence", width=size, height=size, bit_depth=8, primary_image=True,
          frames=[list(c) for c in SEQ_COLOURS], frame_ms=100, repetitions="infinite", expect="decodes",
          notes="Loops forever; also a still primary image (the first frame), so frames: First takes "
          "the primary and frames: All the track (FD18).")

    name = "seq_only.avif"
    if build:
        # The same track with the still image's meta box removed: a sequence-only file (FD16).
        top = boxes(b)
        ftyp = next(x for x in top if x.kind == "ftyp")
        brands = [b"avis", b"msf1", b"iso8", b"isom"]
        new_ftyp = struct.pack(">I4s4sI", 16 + 4 * len(brands), b"ftyp", b"avis", 0) + b"".join(brands)
        parts = [new_ftyp] + [(x.off, x.end) for x in top if x.kind not in ("ftyp", "meta")]
        out = relayout(b, parts)
        (HERE / name).write_bytes(out)
    b2 = (HERE / name).read_bytes()
    assert "meta" not in top_level(b2) and sample_count(b2) == len(flat)
    check_frames(name, tmp, SEQ_COLOURS, size)
    entry(name, kind="sequence", width=size, height=size, bit_depth=8, primary_image=False,
          frames=[list(c) for c in SEQ_COLOURS], frame_ms=100, repetitions="infinite", expect="decodes",
          notes="No meta box, so no primary image: frames: First takes the first frame of the first "
          "track (FD16).")

    name = "seq_hevc.heif"
    six = flat[:6]
    if build:
        encode_sequence(tmp, HERE / name, six, ["-q", "80", "--timebase", "1000", "--duration", "40",
                                                  "--repetitions", "3"])
    b = (HERE / name).read_bytes()
    assert "repetitions: 3" in heif_info(HERE / name) and sample_count(b) == 6
    check_frames(name, tmp, SEQ_COLOURS[:6], size)
    entry(name, kind="sequence", width=size, height=size, bit_depth=8, primary_image=True,
          frames=[list(c) for c in SEQ_COLOURS[:6]], frame_ms=40, repetitions=3, expect="decodes",
          notes="HEVC, played three times: the loop count comes from the repetitions (FD18).")

    name = "seq_over_cap.avif"
    big, count = 4096, 17
    if build:
        # One 4096^2 frame at a time, as uint8: all 17 as float64 would hold 6.8 GB.
        frames = (np.full((big, big, 3), np.array((i * 15, 80, 200 - i * 10), np.uint8)) / 255
                  for i in range(count))
        encode_sequence(tmp, HERE / name, frames, ["-A", "-q", "30", "-p", "speed=9", "--fps", "10"])
    b = (HERE / name).read_bytes()
    assert sample_count(b) == count and (big, big) in ispe_sizes(b)
    entry(name, kind="sequence", width=big, height=big, bit_depth=8, primary_image=True, frame_count=count,
          frame_ms=100, repetitions=1, expect="truncated",
          notes="17 frames of 64 MiB in RGBA8: over the 1 GiB animation cap, so 16 frames and "
          "truncated: true (FD18). Not decoded here, to spare the memory.")


def build_meta_after_mdat(tmp, build):
    """meta after mdat: allowed, and what some writers produce (FD19's reader, not a prefix)."""
    h, w = 192, 256
    png8(tmp / "small.png", marked(h, w))
    for name, codec in (("meta_after_mdat.heic", []), ("meta_after_mdat.avif", ["-A"])):
        if build:
            heif_enc(tmp / "small.png", tmp / "plain", *codec, "-q", "85")
            b = (tmp / "plain").read_bytes()
            top = {x.kind: x for x in boxes(b)}
            out = relayout(b, [(top[k].off, top[k].end) for k in ("ftyp", "mdat", "meta")])
            (HERE / name).write_bytes(out)
        assert top_level((HERE / name).read_bytes()) == ["ftyp", "mdat", "meta"]
        (frame,) = decode(HERE / name, tmp)
        check_corners(name, frame, h, w)
        entry(name, kind="still", width=w, height=h, bit_depth=8, corners=corners_entry(h, w),
              boxes=["ftyp", "mdat", "meta"], expect="decodes")


APPLE_GAIN_MAP = "urn:com:apple:photo:2020:aux:hdrgainmap"
GAIN_HEADROOM = 4.0


def gainmap_sources(portrait):
    """The pictures as shown, with two white patches: `a` and `b`, left and right (top and bottom in
    portrait). The HDR version brightens `b` to 4x SDR white, which the ISO map then carries."""
    h, w = (256, 192) if portrait else (192, 256)
    if portrait:
        rects = {"a": (48, 72), "b": (160, 72)}
    else:
        rects = {"a": (72, 48), "b": (72, 160)}
    sdr = marked(h, w)
    for y, x in rects.values():
        sdr[y:y + 48, x:x + 48] = 1.0
    hdr = srgb_decode(sdr)
    y, x = rects["b"]
    hdr[y:y + 48, x:x + 48] = GAIN_HEADROOM
    return sdr, hdr, rects, h, w


def gain_pixels(h, w, rects, values):
    """Apple's gain map, half the size of the picture: `values` per patch, 0 elsewhere."""
    gain = np.zeros((h // 2, w // 2))
    for key, value in values.items():
        y, x = rects[key]
        gain[y // 2:(y + 48) // 2, x // 2:(x + 48) // 2] = value
    return gain


def to_coded(img, portrait):
    """Coded orientation: a portrait is stored turned anti-clockwise, with EXIF orientation 6
    (irot 270 anti-clockwise) to turn it back."""
    return np.ascontiguousarray(np.rot90(img, k=1)) if portrait else img


def build_gainmaps(tmp, build):
    """SDR base plus gain map, from Apple's encoder: ISO 21496-1 (tmap), Apple's HDR gain map, both,
    and portraits. ImageIO's own HDR decode is recorded as the reference (FD17, step 6c)."""
    tool = None
    if shutil.which("swiftc"):
        tool = tmp / "gainmaps"
        run("swiftc", "-O", HERE / "gainmaps.swift", "-o", tool)
    elif build:
        sys.exit("swiftc is needed to build the gain-map files (macOS 15 or later)")
    cases = [("gainmap_iso.heic", "iso", False, None),
             ("gainmap_iso_portrait.heic", "iso", True, None),
             ("gainmap_apple.heic", "apple", False, {"a": 255, "b": 128}),
             ("gainmap_apple_portrait.heic", "apple", True, {"a": 255, "b": 128}),
             ("gainmap_apple_portrait_unrotated_map.heic", "derived", True, {"a": 255, "b": 128}),
             ("gainmap_both.heic", "both", False, {"a": 255})]
    for name, mode, portrait, gain_values in cases:
        sdr, hdr, rects, h, w = gainmap_sources(portrait)
        if mode == "derived":
            # ImageIO, like an iPhone, gives Apple's gain map the base's irot. This copy drops it:
            # the map's own transforms are ignored (FD17), so it must render the same.
            if build:
                b = (HERE / "gainmap_apple_portrait.heic").read_bytes()
                aux_id = next(i for i, urn in aux_types(b).items() if urn == APPLE_GAIN_MAP)
                (HERE / name).write_bytes(drop_property(b, aux_id, "irot"))
            mode = "apple"
        elif build:
            src = tmp / "gm-src"
            ch, cw = to_coded(sdr, portrait).shape[:2]
            if mode == "apple":
                rgba = np.dstack([to_coded(sdr, portrait), np.ones((ch, cw))])
                src.write_bytes(np.clip(np.rint(rgba * 255), 0, 255).astype(np.uint8).tobytes())
            else:
                rgba = np.dstack([to_coded(hdr, portrait), np.ones((ch, cw))])
                src.write_bytes(rgba.astype("<f4").tobytes())
            args = [mode, src, cw, ch, HERE / name, 6 if portrait else 1, GAIN_HEADROOM]
            if gain_values:
                gain = to_coded(gain_pixels(h, w, rects, gain_values), portrait)
                (tmp / "gm-gain").write_bytes(gain.astype(np.uint8).tobytes())
                args += [tmp / "gm-gain", gain.shape[1], gain.shape[0]]
            run(tool, *args)
        b = (HERE / name).read_bytes()
        kinds, aux = items(b), aux_types(b)
        has_iso = "tmap" in kinds.values() and find(b, "meta", "grpl") is not None
        has_apple = APPLE_GAIN_MAP in aux.values()
        assert has_iso == (mode != "apple") and has_apple == (mode != "iso"), f"{name}: {kinds} {aux}"
        # ImageIO tone-maps an ISO file's SDR base from the HDR picture, desaturating it, so only the
        # nearest corner colour is checked: here the corners show orientation, nothing more.
        (frame,) = decode(HERE / name, tmp)
        check_corners(name, frame, h, w, tolerance=1.0)
        record = {"kind": "gainmap", "width": w, "height": h, "bit_depth": 8,
                  "gain_maps": [k for k, on in (("iso", has_iso), ("apple", has_apple)) if on],
                  "items": {str(i): ("aux:" + aux[i] if i in aux else t) for i, t in kinds.items()},
                  "irot": {str(i): v for i, v in rotations(b).items()},
                  "corners": corners_entry(h, w), "patches": {}, "expect": "decodes"}
        if gain_values:
            record["apple_gain"] = {"headroom_xmp": GAIN_HEADROOM, "size": [w // 2, h // 2],
                                    "values": gain_values, "coded_orientation": True}
        if not tool:
            # --check without Swift: ImageIO's reference values can't be re-measured, so keep them.
            committed = json.loads((HERE / "expected.json").read_text())["files"][name]
            record["patches"], record["imageio_headroom"] = committed["patches"], committed["imageio_headroom"]
        else:
            info = json.loads(run(tool, "decode", HERE / name, tmp / "gm-hdr").stdout)
            hdr_out = np.frombuffer((tmp / "gm-hdr").read_bytes(), "<f4").reshape(
                info["height"], info["width"], 4)
            record["imageio_headroom"] = round(info["headroom"], 4)
            for key, (y, x) in rects.items():
                ys, xs = y + 24, x + 24
                cy, cx = (w - 1 - xs, ys) if portrait else (ys, xs)
                record["patches"][key] = {"y": y, "x": x, "size": 48, "sdr": 1.0,
                                          "imageio_hdr": [round(float(v), 4) for v in hdr_out[cy, cx, :3]]}
        entry(name, **record)

    # ISO's tmap as the primary item, which libheif 1.23 may not fetch: the crate falls back through
    # the altr group to the SDR base (FD17).
    name = "gainmap_iso_tmap_primary.heic"
    if build:
        b = bytearray((HERE / "gainmap_iso.heic").read_bytes())
        pitm = find(b, "meta", "pitm")
        tmap_id = next(i for i, t in items(b).items() if t == "tmap")
        put_uint(b, pitm.body + 4, 2 if b[pitm.body] == 0 else 4, tmap_id)
        (HERE / name).write_bytes(bytes(b))
    b = (HERE / name).read_bytes()
    pitm = find(b, "meta", "pitm")
    assert items(b)[uint(b, pitm.body + 4, 2 if b[pitm.body] == 0 else 4)] == "tmap"
    libheif = run("heif-dec", HERE / name, tmp / "tmap.png", check=False).returncode == 0
    entry(name, kind="gainmap", width=256, height=192, gain_maps=["iso"], primary="tmap",
          libheif_decodes_primary=libheif, expect="decodes",
          notes="gainmap_iso.heic with pitm on the tmap item. Shown as its SDR base (or HDR with "
          "hdr: Linear), found through the altr group.")


def build_bad(tmp, build):
    """Malformed and oversized files: each must come back as an error, never an abort."""
    h, w = 192, 256
    png8(tmp / "small.png", marked(h, w))
    png8(tmp / "grid.png", marked(1024, 1024))
    png16(tmp / "grid16.png", to16(marked(1536, 1536)))
    heif_enc(tmp / "small.png", tmp / "base.heic", "-q", "85")
    heif_enc(tmp / "small.png", tmp / "base.avif", "-A", "-q", "85")
    heif_enc(tmp / "grid.png", tmp / "grid.heic", "-q", "60", "--cut-tiles", "512")
    heif_enc(tmp / "grid16.png", tmp / "grid10.heic", "-q", "60", "-b", "10", "--cut-tiles", "768")
    heic, avif, grid, grid10 = ((tmp / f).read_bytes()
                                for f in ("base.heic", "base.avif", "grid.heic", "grid10.heic"))

    def truncated_mdat(b):
        mdat = find(b, "mdat")
        return b[:mdat.body + (mdat.end - mdat.body) // 2]

    def truncated_meta(b):
        meta = find(b, "meta")
        return b[:meta.off + meta.size // 2]

    def iloc_past_eof(b):
        out = bytearray(b)
        pos, n, _ = next(f for f in iloc_fields(out) if f[2] == "length")
        put_uint(out, pos, n, (1 << (8 * n)) - 16)
        return bytes(out)

    def zero_config(kind):
        def f(b):
            out = bytearray(b)
            box = find(b, "meta", "iprp", "ipco", kind)
            out[box.body:box.end] = bytes(box.end - box.body)
            return bytes(out)
        return f

    def scrambled_data(b):
        """The coded data overwritten with random bytes, its length kept. (Zeroing av1C instead is
        no error: AV1 decoders read their configuration from the bitstream.)"""
        out = bytearray(b)
        mdat = find(b, "mdat")
        out[mdat.body:mdat.end] = np.random.default_rng(11).bytes(mdat.end - mdat.body)
        return bytes(out)

    def ispe(size):
        def f(b):
            out = bytearray(b)
            for box in find_all(out, "ispe"):
                put_uint(out, box.body + 4, 4, size)
                put_uint(out, box.body + 8, 4, size)
            return bytes(out)
        return f

    def grid_descriptor(b):
        idat = find(b, "meta", "idat")
        return idat, b[idat.body + 1] & 1  # flags bit 0: 32-bit output sizes

    def grid_too_few_tiles(b):
        out = bytearray(b)
        idat, _ = grid_descriptor(b)
        out[idat.body + 2] += 2  # rows_minus_one
        out[idat.body + 3] += 2  # columns_minus_one
        return bytes(out)

    def grid_bomb(n, tile, size):
        """n x n references to one tile of a size x size grid: a huge image from a few KB."""
        return lambda b: repeat_tile(b, n, tile, size)

    def repeat_tile(b, n, tile, size):
        out = bytearray(b)
        idat, wide = grid_descriptor(out)
        assert not wide and n * tile < 65536
        out[idat.body + 2] = n - 1
        out[idat.body + 3] = n - 1
        put_uint(out, idat.body + 4, 2, n * tile)
        put_uint(out, idat.body + 6, 2, n * tile)
        for box in find_all(out, "ispe"):
            if (uint(out, box.body + 4, 4), uint(out, box.body + 8, 4)) == (size, size):
                put_uint(out, box.body + 4, 4, n * tile)
                put_uint(out, box.body + 8, 4, n * tile)
        out = bytes(out)
        iref = find(out, "meta", "iref")
        version = out[iref.body]
        idn = 2 if version == 0 else 4
        refs = children(out, iref)
        body = bytearray(out[iref.body:iref.body + 4])
        for ref in refs:
            data = out[ref.body:ref.end]
            if ref.kind == "dimg":
                first_tile = data[idn + 2:idn + 2 + idn]
                data = data[:idn] + struct.pack(">H", n * n) + first_tile * (n * n)
            body += struct.pack(">I4s", 8 + len(data), ref.kind.encode()) + data
        old_mdat = find(out, "mdat")
        resized = resize_box(out, iref, bytes(body))
        shift = len(resized) - len(out)
        fixed = bytearray(resized)
        for pos, n_bytes, kind in iloc_fields(fixed):
            if kind != "length":
                value = uint(fixed, pos, n_bytes)
                if value >= old_mdat.off:
                    put_uint(fixed, pos, n_bytes, value + shift)
        return bytes(fixed)

    cases = [
        ("bad_truncated_mdat.heic", heic, truncated_mdat, "error", "Cut halfway through the coded data."),
        ("bad_truncated_meta.heic", heic, truncated_meta, "error", "Cut halfway through the meta box."),
        ("bad_iloc_past_eof.heic", heic, iloc_past_eof, "error", "An extent reaching ~4 GB past the end."),
        ("bad_hvcC.heic", heic, zero_config("hvcC"), "error", "The HEVC decoder configuration zeroed."),
        ("bad_av1_data.avif", avif, scrambled_data, "error", "The AV1 data replaced by random bytes."),
        ("bad_grid_tiles.heic", grid, grid_too_few_tiles, "error",
         "The grid asks for 4x4 tiles and has 2x2."),
        ("bad_not_heif.heic", heic, lambda b: find_ftyp(b) + np.random.default_rng(7).bytes(4096),
         "error", "A HEIC ftyp followed by random bytes."),
        ("bad_ftyp_only.heic", heic, find_ftyp, "error", "Nothing after the ftyp box."),
        ("big_ispe_40000.heic", heic, ispe(40000), "too_large",
         "ispe says 40000x40000 (coded 256x192): over texture() / thumbnail()'s 30000 px per axis, "
         "and over viewer()'s 4 GiB as RGBA8. libheif's default limit (2^30 pixels) refuses it too."),
        ("big_ispe_40000.avif", avif, ispe(40000), "too_large", "The same, AVIF."),
        ("big_ispe_200000.heic", heic, ispe(200000), "too_large",
         "ispe says 200000x200000: over every setup's per-axis limit, viewer()'s 131072 included."),
        ("bomb_grid_refs.heic", grid, grid_bomb(80, 512, 1024), "too_large",
         "An 80x80 grid of one 512 px tile: 40960x40960, 6.25 GiB as RGBA8. libheif's default limit "
         "of 1000 iref references refuses it before any size check."),
        ("bomb_grid_bytes.heic", grid10, grid_bomb(31, 768, 1536), "too_large",
         "A 31x31 grid of one 10-bit 768 px tile: 23808x23808, within every per-axis limit and "
         "libheif's defaults (961 references, 2^30 pixels), but 4.2 GiB as RGBA16 and 2.1 GiB as "
         "RGBA8: only a byte limit catches it, in every setup."),
    ]
    for name, source, mutate, expect, note in cases:
        if build:
            (HERE / name).write_bytes(mutate(source))
        b = (HERE / name).read_bytes()
        claimed = {"big_ispe_40000.heic": 40000, "big_ispe_40000.avif": 40000,
                   "big_ispe_200000.heic": 200000, "bomb_grid_refs.heic": 40960,
                   "bomb_grid_bytes.heic": 23808}
        if name in claimed:
            sizes = ispe_sizes(b)
            assert max(sizes) == (claimed[name], claimed[name]), f"{name}: ispe {sizes}"
            # Not decoded here: a decoder without limits would try to allocate the size it claims.
            entry(name, kind="oversized", width=claimed[name], height=claimed[name], expect=expect,
                  notes=note)
        else:
            result = run("heif-dec", HERE / name, tmp / "bad.png", check=False)
            assert result.returncode != 0, f"{name}: libheif decoded it"
            entry(name, kind="malformed", expect=expect, notes=note)


def find_ftyp(b):
    ftyp = find(b, "ftyp")
    return b[ftyp.off:ftyp.end]


# ---------------------------------------------------------------------------------------------


def main():
    build = "--check" not in sys.argv[1:]
    missing = [t for t in TOOLS if not shutil.which(t)]
    if missing:
        sys.exit(f"missing tools: {', '.join(missing)}")
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for step in (build_depths, build_grid, build_oriented, build_hdr, build_p3, build_premultiplied,
                     build_sequences, build_meta_after_mdat, build_gainmaps, build_bad):
            step(tmp, build)
            print(f"{step.__name__}: ok")
    versions = run("heif-enc", "--version").stdout.split()[0]
    encoders = [line.strip("- ") for line in run("heif-enc", "--list-encoders").stdout.splitlines()
                if line.startswith("- ")]
    record = {"made_with": {"libheif": versions, "encoders": encoders}, "files": EXPECTED}
    if build:
        (HERE / "expected.json").write_text(json.dumps(record, indent=1) + "\n")
    else:
        committed = json.loads((HERE / "expected.json").read_text())["files"]
        assert committed == json.loads(json.dumps(EXPECTED)), "expected.json differs from the checks"
    print(f"{len(EXPECTED)} files ok")


if __name__ == "__main__":
    main()
