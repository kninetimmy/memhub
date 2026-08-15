#!/usr/bin/env python3
"""memhub hybrid-recall diagram, Style 8 Dark Luxury, hand-built topology.

Static SVG + per-frame SVGs with computed dot positions (no SMIL, no tails).
Usage:
  python hybrid-recall-motion-gen.py static out.svg
  python hybrid-recall-motion-gen.py frames outdir/   # frame_000.svg .. frame_114.svg
"""
import math
import sys
import os

W, H = 960, 560
FRAMES = 115

# Style 8 tokens
BG = "#0a0a0a"
SURF = "#111111"
GOLD = "#d4a574"
GOLD_DIM = "#c9a96e"
GOLD_BRIGHT = "#e8c49a"
TXT = "#f5f0eb"
TXT2 = "#a39787"
TXT3 = "#6b5f53"
MINT = "#6ee7b7"
ORANGE = "#fdba74"
GRAY = "#94a3b8"
ROSE = "#f87171"
VIOLET = "#a78bfa"
GREEN = "#5a9e6f"
SKY = "#38bdf8"
AMBER = "#fbbf24"

SANS = "-apple-system,'Helvetica Neue',Arial,sans-serif"
SERIF = "Georgia,'Times New Roman',serif"
MONO = "'Cascadia Code','SF Mono','Courier New',monospace"

# ---- geometry -------------------------------------------------------------

PATHS = {
    "qsplit":  [(480, 86), (480, 112)],
    "tofts":   [(480, 112), (240, 112), (240, 152)],
    "tovec":   [(480, 112), (720, 112), (720, 152)],
    "ftsout":  [(240, 224), (240, 268), (474, 268)],
    "vecout":  [(720, 224), (720, 268), (486, 268)],
    "merge":   [(480, 268), (480, 296)],
    "torank":  [(480, 356), (480, 388)],
    "toout":   [(480, 448), (480, 480)],
}

EDGES = [
    # name, color, width, dash, marker_end
    ("qsplit", MINT,   1.5, None, None),
    ("tofts",  SKY,    1.5, None, "arr-sky"),
    ("tovec",  VIOLET, 1.5, None, "arr-violet"),
    ("ftsout", SKY,    1.5, None, None),
    ("vecout", VIOLET, 1.5, None, None),
    ("merge",  MINT,   2,   None, "arr-mint"),
    ("torank", MINT,   2,   None, "arr-mint"),
    ("toout",  GOLD,   2,   None, "arr-gold"),
]

# ---- helpers --------------------------------------------------------------

def path_d(pts):
    d = f"M {pts[0][0]},{pts[0][1]}"
    for x, y in pts[1:]:
        d += f" L {x},{y}"
    return d

def point_at(pts, t):
    """Arc-length position along polyline, t in [0,1]. Clamped, exact at corners."""
    t = max(0.0, min(1.0, t))
    segs = []
    total = 0.0
    for a, b in zip(pts, pts[1:]):
        L = math.hypot(b[0] - a[0], b[1] - a[1])
        segs.append((a, b, L))
        total += L
    target = t * total
    run = 0.0
    for a, b, L in segs:
        if run + L >= target or (a, b, L) == segs[-1]:
            f = 0.0 if L == 0 else (target - run) / L
            f = max(0.0, min(1.0, f))
            return (a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f)
        run += L
    return pts[-1]

def smooth(u):
    u = max(0.0, min(1.0, u))
    return u * u * (3 - 2 * u)

def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

# ---- svg builders ---------------------------------------------------------

def node(L, x, y, w, h, stroke, name, subs, name_fill=None, mono_name=False):
    L.append(f'  <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" '
             f'fill="{SURF}" stroke="{stroke}" stroke-width="1.5" data-graph-role="node"/>')
    fam = MONO if mono_name else SANS
    L.append(f'  <text x="{x+12}" y="{y+22}" font-family="{fam}" font-size="13" '
             f'font-weight="600" fill="{name_fill or stroke}">{esc(name)}</text>')
    yy = y + 40
    for sub in subs:
        L.append(f'  <text x="{x+12}" y="{yy}" font-family="{SANS}" font-size="10" '
                 f'fill="{TXT2}">{esc(sub)}</text>')
        yy += 16

def pulse_rect(L, x, y, w, h, color, op, width=1.5):
    L.append(f'  <rect x="{x-3}" y="{y-3}" width="{w+6}" height="{h+6}" rx="8" '
             f'fill="none" stroke="{color}" stroke-width="{width}" opacity="{op:.2f}"/>')

def build_svg(dots=None, fts_pulse=0.0, vec_pulse=0.0, blend_pulse=0.0,
              rank_pulse=0.0, out_glow=0.0):
    L = []
    L.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}">')
    L.append('  <defs>')
    for mid, col in [("arr-sky", SKY), ("arr-violet", VIOLET),
                     ("arr-mint", MINT), ("arr-gold", GOLD)]:
        L.append(f'    <marker id="{mid}" markerUnits="userSpaceOnUse" markerWidth="11" '
                 f'markerHeight="8" refX="10" refY="4" orient="auto-start-reverse">'
                 f'<polygon points="0 0,11 4,0 8" fill="{col}"/></marker>')
    L.append('    <radialGradient id="glow" cx="50%" cy="45%" r="55%">')
    L.append(f'      <stop offset="0%" stop-color="{GOLD}" stop-opacity="0.05"/>')
    L.append(f'      <stop offset="100%" stop-color="{GOLD}" stop-opacity="0"/>')
    L.append('    </radialGradient>')
    L.append('  </defs>')
    L.append(f'  <rect width="{W}" height="{H}" fill="{BG}"/>')
    L.append(f'  <rect width="{W}" height="{H}" fill="url(#glow)"/>')

    # title (top-right)
    L.append(f'  <text x="920" y="44" text-anchor="end" font-family="{SERIF}" '
             f'font-size="22" font-weight="700" fill="{TXT}">hybrid recall</text>')
    L.append(f'  <text x="920" y="62" text-anchor="end" font-family="{SANS}" '
             f'font-size="10.5" fill="{TXT2}">one query &#183; two searches &#183; '
             f'one ranked answer</text>')

    # arrows (before nodes so nodes sit on top of endpoints)
    for name, col, wd, dash, mend in EDGES:
        d = path_d(PATHS[name])
        attrs = f'fill="none" stroke="{col}" stroke-width="{wd}"'
        if dash:
            attrs += f' stroke-dasharray="{dash}"'
        if mend:
            attrs += f' marker-end="url(#{mend})"'
        L.append(f'  <path d="{d}" {attrs} data-graph-role="edge"/>')

    # arrow labels
    def albl(x, y, text, mono=False, anchor="middle", size=10):
        fam = MONO if mono else SANS
        L.append(f'  <text x="{x}" y="{y}" text-anchor="{anchor}" font-family="{fam}" '
                 f'font-size="{size}" fill="#8c7e72">{esc(text)}</text>')

    albl(196, 250, "BM25 score", mono=True)
    albl(766, 250, "cosine score", mono=True)
    albl(480, 246, "both run over project.sqlite - locally", size=9)

    # query node
    node(L, 330, 36, 300, 50, MINT, 'recall("why did we pick rusqlite?")',
         [], name_fill=TXT, mono_name=True)

    # branch nodes
    node(L, 110, 152, 260, 72, SKY, "FTS5 - BM25",
         ["keyword + stemmed match", "finds the exact words"])
    if fts_pulse > 0:
        pulse_rect(L, 110, 152, 260, 72, SKY, 0.55 * fts_pulse)
    node(L, 590, 152, 260, 72, VIOLET, "BGE-small vectors",
         ["semantic similarity - cosine", "finds the meaning, not the words"])
    if vec_pulse > 0:
        pulse_rect(L, 590, 152, 260, 72, VIOLET, 0.55 * vec_pulse)

    # blend node
    node(L, 330, 296, 300, 60, GREEN, "score blend",
         ["0.5 x fts + 0.5 x vec - 0.3 x stale"])
    if blend_pulse > 0:
        pulse_rect(L, 330, 296, 300, 60, MINT, 0.55 * blend_pulse)

    # re-ranker node
    node(L, 330, 388, 300, 60, AMBER, "cross-encoder re-rank",
         ["ms-marco-MiniLM - scores top 20 pairs"])
    if rank_pulse > 0:
        pulse_rect(L, 330, 388, 300, 60, AMBER, 0.55 * rank_pulse)

    # output node
    node(L, 330, 480, 300, 56, GOLD, "ranked bundle",
         ["cited - scored - stale-flagged"], name_fill=GOLD_BRIGHT)
    if out_glow > 0:
        pulse_rect(L, 330, 480, 300, 56, GOLD_BRIGHT, 0.25 + 0.45 * out_glow)

    # legend
    L.append(f'  <rect x="36" y="452" width="252" height="106" rx="8" fill="none" '
             f'stroke="{GOLD}" stroke-width="0.5" stroke-dasharray="6,4" opacity="0.35"/>')
    entries = [
        (SKY, "keyword search"),
        (VIOLET, "semantic search"),
        (MINT, "blended candidates"),
        (GOLD, "what the agent gets back"),
    ]
    yy = 474
    for col, txt in entries:
        L.append(f'  <line x1="46" y1="{yy}" x2="74" y2="{yy}" stroke="{col}" '
                 f'stroke-width="1.5"/>')
        L.append(f'  <text x="82" y="{yy+3}" font-family="{SANS}" font-size="10" '
                 f'fill="{TXT2}">{esc(txt)}</text>')
        yy += 24

    # bottom-right note
    L.append(f'  <text x="920" y="552" text-anchor="end" font-family="{SANS}" '
             f'font-size="9" fill="{TXT3}">models bundled in the binary - '
             f'runs offline - no network</text>')

    # dots overlay (topmost)
    if dots:
        for (x, y, col, r, op) in dots:
            L.append(f'  <circle cx="{x:.2f}" cy="{y:.2f}" r="{r*2.2:.2f}" '
                     f'fill="{col}" opacity="{0.18*op:.3f}"/>')
            L.append(f'  <circle cx="{x:.2f}" cy="{y:.2f}" r="{r:.2f}" '
                     f'fill="{col}" opacity="{op:.3f}"/>')

    L.append('</svg>')
    return "\n".join(L)

# ---- animation timeline ---------------------------------------------------

# journeys: (path pts, f0, f1, color, radius)
JOURNEYS = [
    (PATHS["qsplit"], 2, 8, MINT, 4),
    (PATHS["tofts"], 8, 18, SKY, 4),
    (PATHS["tovec"], 8, 18, VIOLET, 4),
    (PATHS["ftsout"], 26, 38, SKY, 4),
    (PATHS["vecout"], 28, 40, VIOLET, 4),
    (PATHS["merge"], 40, 46, MINT, 4.5),
    (PATHS["torank"], 52, 58, MINT, 4.5),
    (PATHS["toout"], 66, 72, GOLD_BRIGHT, 4.5),
    (PATHS["toout"], 70, 76, GOLD_BRIGHT, 3.5),
    (PATHS["toout"], 74, 80, GOLD_BRIGHT, 3),
]

def tri(f, c0, c1):
    """Triangle pulse: 0 at c0/c1, 1 at midpoint."""
    if c0 <= f <= c1:
        mid = (c0 + c1) / 2
        return 1.0 - abs(f - mid) / ((c1 - c0) / 2)
    return 0.0

def frame_state(f):
    dots = []
    for pts, f0, f1, col, r in JOURNEYS:
        if f0 <= f <= f1:
            t = smooth((f - f0) / (f1 - f0))
            x, y = point_at(pts, t)
            op = min(1.0, (f - f0) / 1.5, (f1 - f) / 1.5 + 0.34)
            op = max(0.0, min(1.0, op))
            dots.append((x, y, col, r, op))
    out_glow = 0.0
    if 78 <= f <= 96:
        out_glow = tri(f, 78, 96)
    return dict(dots=dots,
                fts_pulse=tri(f, 18, 26),
                vec_pulse=tri(f, 18, 26),
                blend_pulse=tri(f, 44, 52),
                rank_pulse=tri(f, 56, 66),
                out_glow=out_glow)

# ---- main -----------------------------------------------------------------

if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "static":
        out = sys.argv[2]
        with open(out, "w", encoding="utf-8") as fh:
            fh.write(build_svg())
        print(f"wrote {out}")
    elif mode == "frames":
        outdir = sys.argv[2]
        os.makedirs(outdir, exist_ok=True)
        for f in range(FRAMES):
            svg = build_svg(**frame_state(f))
            with open(os.path.join(outdir, f"frame_{f:03d}.svg"), "w",
                      encoding="utf-8") as fh:
                fh.write(svg)
        print(f"wrote {FRAMES} frames to {outdir}")
