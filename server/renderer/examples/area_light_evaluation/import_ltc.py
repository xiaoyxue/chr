"""Repack the pinned, licensed public LTC tables; run from the workspace root."""

import hashlib
import pathlib
import re
import struct
import urllib.request

REVISION = "31e5e96b54f98f33098f8503003119ba2231a1c6"
BASE = f"https://raw.githubusercontent.com/selfshadow/ltc_code/{REVISION}/"
DEST = pathlib.Path("server/renderer/src/ltc")
source = urllib.request.urlopen(BASE + "fit/results/ltc.js").read()
assert hashlib.sha256(source).hexdigest() == (
    "21071160163defd419b8f754ab604a8e60c44fcf539bfff9916e0e8e82316c1d"
)
tables = re.findall(rb"var g_ltc_[12] = \[(.*?)\];", source, re.S)
values = [[float(v) for v in table.split(b",") if v.strip()] for table in tables]
assert len(values) == 2 and all(len(table) == 64 * 64 * 4 for table in values)
# Keep the inverse matrix and the two magnitude/Fresnel terms, not clipless data.
packed = b"".join(
    struct.pack("<6f", *values[0][i : i + 4], *values[1][i : i + 2])
    for i in range(0, 64 * 64 * 4, 4)
)
(DEST / "ltc.bin").write_bytes(packed)
notice = urllib.request.urlopen(BASE + "LICENSE").read().decode()
(DEST / "LICENSE.ltc").write_text("\n".join(line.rstrip() for line in notice.splitlines()) + "\n")
print(f"revision={REVISION}")
print(f"upstream_js_sha256={hashlib.sha256(source).hexdigest()}")
print(f"packed_sha256={hashlib.sha256(packed).hexdigest()} bytes={len(packed)}")
