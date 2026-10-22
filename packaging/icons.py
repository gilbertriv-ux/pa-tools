#!/usr/bin/env python3
"""Pack PNG icons into a Windows .ico or a macOS .icns. Standard library only, so icons.sh works on
Linux and CI without macOS' iconutil or a cargo build.

    python3 packaging/icons.py ico  OUT.ico  16.png 32.png ...      PNG sizes <= 256, stored as is
    python3 packaging/icons.py icns OUT.icns "Pa Tools.iconset"     icon_16x16.png ... icon_512x512@2x.png

`ico` writes the same layout as `cargo xtask ico` (xtask/src/ico.rs): PNG-compressed entries, 256 px
stored as 0. `icns` writes PNG entries (icp4 ... ic14). `iconutil -c icns` on a Mac also adds the
legacy 16/32 px ARGB entries and an info block; both load the same, and the Mac result is canonical.
"""

import os
import struct
import sys

SIG = b"\x89PNG\r\n\x1a\n"
# iconset file name -> ICNS entry type (PNG payload) and the pixel size it must have
ICNS_TYPES = {
    "icon_16x16.png": ("icp4", 16),
    "icon_16x16@2x.png": ("ic11", 32),
    "icon_32x32.png": ("icp5", 32),
    "icon_32x32@2x.png": ("ic12", 64),
    "icon_128x128.png": ("ic07", 128),
    "icon_128x128@2x.png": ("ic13", 256),
    "icon_256x256.png": ("ic08", 256),
    "icon_256x256@2x.png": ("ic14", 512),
    "icon_512x512.png": ("ic09", 512),
    "icon_512x512@2x.png": ("ic10", 1024),
}


def png_size(data, name):
    if len(data) < 24 or data[:8] != SIG or data[12:16] != b"IHDR":
        sys.exit(f"error: {name} is not a PNG file")
    return struct.unpack(">II", data[16:24])


def pack_ico(out, pngs):
    blobs = []
    for p in pngs:
        data = open(p, "rb").read()
        w, h = png_size(data, p)
        if w != h or w == 0 or w > 256:
            sys.exit(f"error: icon images must be square and at most 256 px, got {w}x{h} ({p})")
        blobs.append((w, data))
    head = struct.pack("<HHH", 0, 1, len(blobs))
    offset = 6 + 16 * len(blobs)
    entries = b""
    for w, data in blobs:
        dim = 0 if w == 256 else w
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open(out, "wb") as f:
        f.write(head + entries + b"".join(d for _, d in blobs))
    print(f"{out}: {len(blobs)} images, {offset} bytes")


def pack_icns(out, iconset):
    body = b""
    count = 0
    for name, (kind, px) in ICNS_TYPES.items():
        path = os.path.join(iconset, name)
        if not os.path.exists(path):
            sys.exit(f"error: {path} is missing")
        data = open(path, "rb").read()
        w, h = png_size(data, path)
        if (w, h) != (px, px):
            sys.exit(f"error: {path} is {w}x{h}, expected {px}x{px}")
        body += kind.encode("ascii") + struct.pack(">I", 8 + len(data)) + data
        count += 1
    with open(out, "wb") as f:
        f.write(b"icns" + struct.pack(">I", 8 + len(body)) + body)
    print(f"{out}: {count} images, {8 + len(body)} bytes")


def main(argv):
    if len(argv) >= 4 and argv[1] == "ico":
        pack_ico(argv[2], argv[3:])
    elif len(argv) == 4 and argv[1] == "icns":
        pack_icns(argv[2], argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
