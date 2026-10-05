#!/usr/bin/env python3
"""Builds app/assets/fonts from the upstream OFL fonts (reproducible).

- IBM Plex Mono Regular/Medium/SemiBold: copied as-is.
- Noto Sans TC: upstream ships one *variable* font (wght 100-900, default 100 = Thin,
  ~12 MB). We instantiate static Regular(400) and Medium(500) and subset to the Big5
  character set (Traditional Chinese, levels 1+2) plus punctuation/ASCII.

Usage: python3 tools/build_fonts.py [--check-text FILE]
  --check-text  file with one UI string per line; reports characters no bundled font covers.
Needs: fonttools (pip install fonttools), network access to github.com.
"""
import argparse, hashlib, io, sys, urllib.request
from pathlib import Path
from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

BASE = "https://github.com/google/fonts/raw/main/ofl"
OUT = Path(__file__).resolve().parent.parent / "app" / "assets" / "fonts"

PLEX = ["IBMPlexMono-Regular.ttf", "IBMPlexMono-Medium.ttf", "IBMPlexMono-SemiBold.ttf"]
NOTO_VAR = f"{BASE}/notosanstc/NotoSansTC%5Bwght%5D.ttf"
WEIGHTS = {"Regular": 400, "Medium": 500}


def fetch(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as r:
        return r.read()


def big5_codepoints() -> set[int]:
    cps = set(range(0x20, 0x7F))
    for lead in range(0xA1, 0xFA):
        for trail in list(range(0x40, 0x7F)) + list(range(0xA1, 0xFF)):
            try:
                cps.add(ord(bytes([lead, trail]).decode("big5")))
            except (UnicodeDecodeError, ValueError):
                pass
    # General punctuation, CJK symbols, fullwidth forms, arrows used by the UI.
    for lo, hi in [(0x2000, 0x206F), (0x2190, 0x21FF), (0x3000, 0x303F), (0xFF00, 0xFFEF)]:
        cps.update(range(lo, hi + 1))
    return cps


def build_noto(var_bytes: bytes, weight: int, cps: set[int]) -> bytes:
    font = TTFont(io.BytesIO(var_bytes))
    inst = instancer.instantiateVariableFont(font, {"wght": weight}, updateFontNames=True)
    opts = subset.Options()
    opts.layout_features = ["kern", "locl"]
    opts.hinting = False
    opts.name_IDs = ["*"]
    opts.notdef_outline = True
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=cps)
    sub.subset(inst)
    buf = io.BytesIO()
    inst.save(buf)
    return buf.getvalue()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check-text")
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)

    for name in PLEX:
        data = fetch(f"{BASE}/ibmplexmono/{name}")
        (OUT / name).write_bytes(data)
    (OUT / "OFL-IBMPlexMono.txt").write_bytes(fetch(f"{BASE}/ibmplexmono/OFL.txt"))
    (OUT / "OFL-NotoSansTC.txt").write_bytes(fetch(f"{BASE}/notosanstc/OFL.txt"))

    var_bytes = fetch(NOTO_VAR)
    cps = big5_codepoints()
    for style, wght in WEIGHTS.items():
        (OUT / f"NotoSansTC-{style}.ttf").write_bytes(build_noto(var_bytes, wght, cps))

    print(f"{'bytes':>9}  sha256[:12]  file")
    for p in sorted(OUT.glob("*.ttf")):
        d = p.read_bytes()
        print(f"{len(d):>9}  {hashlib.sha256(d).hexdigest()[:12]}  {p.name}")

    if args.check_text:
        covered: set[int] = set()
        for p in OUT.glob("*.ttf"):
            covered.update(TTFont(p).getBestCmap().keys())
        wanted = {ord(c) for line in Path(args.check_text).read_text().splitlines() for c in line if not c.isspace()}
        missing = sorted(wanted - covered)
        print(f"\ncharacters in UI text: {len(wanted)}; not covered by bundled fonts: {len(missing)}")
        for cp in missing:
            print(f"  U+{cp:04X} {chr(cp)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
