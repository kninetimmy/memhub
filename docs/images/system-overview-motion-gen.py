#!/usr/bin/env python3
"""memhub system-overview diagram, Style 8 Dark Luxury, hand-built topology.

Static SVG + per-frame SVGs with computed dot positions (no SMIL, no tails).
Usage:
  python gen.py static out.svg
  python gen.py frames outdir/          # writes frame_000.svg .. frame_114.svg
"""
import math
import sys
import os

W, H = 960, 600
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
# DB cylinder
CYL_CX, CYL_RX = 845, 75
CYL_TOP, CYL_BOT, CYL_RY = 198, 286, 14

def cyl_bottom_y(x):
    dx = (x - CYL_CX) / CYL_RX
    return CYL_BOT + CYL_RY * math.sqrt(max(0.0, 1 - dx * dx))

PATHS = {
    "call1":   [(205, 192), (250, 192)],
    "call2":   [(205, 252), (250, 252)],
    "call3":   [(205, 312), (250, 312)],
    "query":   [(319, 190), (319, 135), (430, 135)],
    "dbread":  [(845, 184), (845, 135), (660, 135)],
    "bundle":  [(545, 96), (545, 60), (120, 60), (120, 140)],
    "propose": [(319, 330), (319, 380), (430, 380)],
    "s2g":     [(580, 380), (626, 380)],
    "gate2db": [(762, 380), (800, 380), (800, cyl_bottom_y(800))],
    "direct":  [(388, 270), (770, 270)],
    "renderp": [(845, cyl_bottom_y(845)), (845, 430), (645, 430), (645, 490)],
    "syncp":   [(890, cyl_bottom_y(890)), (890, 490)],
}

EDGES = [
    # name, color, width, dash, marker_end, marker_start
    ("call1",   VIOLET, 2,    None,  "arr-violet", None),
    ("call2",   VIOLET, 2,    None,  "arr-violet", None),
    ("call3",   VIOLET, 2,    None,  "arr-violet", None),
    ("query",   MINT,   1.5,  None,  "arr-mint",   None),
    ("dbread",  MINT,   1.5,  None,  "arr-mint",   None),
    ("bundle",  MINT,   2,    None,  "arr-mint",   None),
    ("propose", ORANGE, 1.5,  None,  "arr-orange", None),
    ("s2g",     ORANGE, 1.5,  None,  "arr-orange", None),
    ("gate2db", ORANGE, 1.5,  None,  "arr-orange", None),
    ("direct",  GRAY,   1.25, "4,3", "arr-gray",   None),
    ("renderp", GRAY,   1.25, "4,3", "arr-gray",   None),
    ("syncp",   GRAY,   1.25, "4,3", "arr-gray",   "arr-gray"),
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

def build_svg(dots=None, gate_pulse=0.0, gate_flash=0.0, engine_pulse=0.0,
              db_pulse=0.0, sync_pulse=0.0):
    L = []
    L.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}">')
    L.append('  <defs>')
    for mid, col in [("arr-violet", VIOLET), ("arr-mint", MINT),
                     ("arr-orange", ORANGE), ("arr-gray", GRAY)]:
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
             f'font-size="22" font-weight="700" fill="{TXT}">memhub</text>')
    L.append(f'  <text x="920" y="62" text-anchor="end" font-family="{SANS}" '
             f'font-size="10.5" fill="{TXT2}">local-first project memory &#183; '
             f'agents propose, you approve</text>')

    # agents container
    L.append(f'  <rect x="36" y="140" width="168" height="222" rx="8" fill="none" '
             f'stroke="{GOLD}" stroke-width="0.5" stroke-dasharray="6,4" opacity="0.4"/>')
    L.append(f'  <text x="48" y="158" font-family="{SERIF}" font-size="11" '
             f'font-weight="700" fill="{GOLD_DIM}" opacity="0.75">AGENTS</text>')
    L.append(f'  <text x="120" y="352" text-anchor="middle" font-family="{SANS}" '
             f'font-size="9" fill="{TXT3}">untrusted writers</text>')

    # arrows (before nodes so nodes sit on top of endpoints)
    for name, col, wd, dash, mend, mstart in EDGES:
        d = path_d(PATHS[name])
        attrs = f'fill="none" stroke="{col}" stroke-width="{wd}"'
        if dash:
            attrs += f' stroke-dasharray="{dash}"'
        if mend:
            attrs += f' marker-end="url(#{mend})"'
        if mstart:
            attrs += f' marker-start="url(#{mstart})"'
        L.append(f'  <path d="{d}" {attrs} data-graph-role="edge"/>')

    # arrow labels
    def albl(x, y, text, mono=False, anchor="middle", size=10):
        fam = MONO if mono else SANS
        L.append(f'  <text x="{x}" y="{y}" text-anchor="{anchor}" font-family="{fam}" '
                 f'font-size="{size}" fill="#8c7e72">{esc(text)}</text>')

    albl(373, 127, "recall(query)", mono=True)
    albl(752, 127, "read-only")
    albl(330, 52, "ranked context bundle")
    albl(373, 372, "propose_*", mono=True)
    albl(781, 372, "accept")
    albl(579, 262, "tasks / notes / commands - no gate")
    albl(745, 444, "memhub render", mono=True)
    albl(886, 456, "snapshot / adopt", anchor="end")

    # agent nodes
    for label, y in [("Claude Code", 170), ("Codex", 230), ("OpenCode", 290)]:
        node(L, 48, y, 144, 44, ROSE, label, [])

    # MCP node
    node(L, 250, 190, 138, 140, VIOLET, "MCP server / CLI",
         ["stdio (rmcp) + clap CLI", "read tools: recall, locate",
          "writes: staged + direct"])

    # engine node
    ep = 1.0 + 0.0 * engine_pulse
    estroke = GREEN if engine_pulse <= 0 else GOLD_BRIGHT
    node(L, 430, 96, 230, 78, GREEN, "Hybrid recall",
         ["FTS5 BM25 + BGE-small vectors", "blend → cross-encoder re-rank"])
    if engine_pulse > 0:
        L.append(f'  <rect x="428" y="94" width="234" height="82" rx="7" fill="none" '
                 f'stroke="{MINT}" stroke-width="1.5" opacity="{0.55*engine_pulse:.2f}"/>')

    # pending_writes node
    node(L, 430, 350, 150, 60, AMBER, "pending_writes",
         ["staged proposals"], mono_name=True)

    # review gate node
    node(L, 626, 344, 136, 72, GOLD, "Review gate",
         ["memhub review accept", "human approval"], name_fill=GOLD_BRIGHT)
    if gate_pulse > 0:
        L.append(f'  <rect x="623" y="341" width="142" height="78" rx="8" fill="none" '
                 f'stroke="{GOLD_BRIGHT}" stroke-width="1.5" '
                 f'opacity="{(0.25 + 0.5 * gate_pulse):.2f}"/>')
    if gate_flash > 0:
        L.append(f'  <rect x="626" y="344" width="136" height="72" rx="6" '
                 f'fill="{GOLD_BRIGHT}" opacity="{0.28*gate_flash:.2f}"/>')

    # DB cylinder
    x0, x1 = CYL_CX - CYL_RX, CYL_CX + CYL_RX
    L.append(f'  <path d="M {x0},{CYL_TOP} A {CYL_RX},{CYL_RY} 0 0 0 {x1},{CYL_TOP} '
             f'L {x1},{CYL_BOT} A {CYL_RX},{CYL_RY} 0 0 1 {x0},{CYL_BOT} Z" '
             f'fill="{SURF}" stroke="{SKY}" stroke-width="1.5" data-graph-role="node"/>')
    L.append(f'  <ellipse cx="{CYL_CX}" cy="{CYL_TOP}" rx="{CYL_RX}" ry="{CYL_RY}" '
             f'fill="{SURF}" stroke="{SKY}" stroke-width="1.5"/>')
    if db_pulse > 0:
        L.append(f'  <path d="M {x0-3},{CYL_TOP} A {CYL_RX+3},{CYL_RY+2} 0 0 0 '
                 f'{x1+3},{CYL_TOP} L {x1+3},{CYL_BOT} A {CYL_RX+3},{CYL_RY+2} 0 0 1 '
                 f'{x0-3},{CYL_BOT} Z" fill="none" stroke="{SKY}" stroke-width="1.5" '
                 f'opacity="{0.6*db_pulse:.2f}"/>')
    L.append(f'  <text x="{CYL_CX}" y="238" text-anchor="middle" font-family="{MONO}" '
             f'font-size="13" font-weight="600" fill="{SKY}">project.sqlite</text>')
    L.append(f'  <text x="{CYL_CX}" y="256" text-anchor="middle" font-family="{SANS}" '
             f'font-size="10" fill="{TXT2}">source of truth</text>')
    L.append(f'  <text x="{CYL_CX}" y="271" text-anchor="middle" font-family="{SANS}" '
             f'font-size="9" fill="{TXT3}">facts &#183; decisions &#183; tasks &#183; docs</text>')

    # rendered view + drive sync
    node(L, 560, 490, 170, 52, GRAY, "PROJECT.md",
         ["rendered view - one-way"], mono_name=True)
    node(L, 790, 490, 150, 52, GRAY, "Drive sync",
         ["snapshot / adopt (gated)"])
    if sync_pulse > 0:
        L.append(f'  <rect x="787" y="487" width="156" height="58" rx="7" fill="none" '
                 f'stroke="{GRAY}" stroke-width="1.25" opacity="{0.55*sync_pulse:.2f}"/>')

    # legend
    L.append(f'  <rect x="36" y="470" width="344" height="106" rx="8" fill="none" '
             f'stroke="{GOLD}" stroke-width="0.5" stroke-dasharray="6,4" opacity="0.35"/>')
    entries = [
        (VIOLET, 2, None, "agent call - MCP tools / CLI"),
        (MINT, 1.5, None, "read path - hybrid recall (read-only)"),
        (ORANGE, 1.5, None, "staged write - durable only after review accept"),
        (GRAY, 1.25, "4,3", "low-risk direct writes / render / sync"),
    ]
    yy = 492
    for col, wd, dash, txt in entries:
        dd = f' stroke-dasharray="{dash}"' if dash else ""
        L.append(f'  <line x1="46" y1="{yy}" x2="74" y2="{yy}" stroke="{col}" '
                 f'stroke-width="{wd}"{dd}/>')
        L.append(f'  <text x="82" y="{yy+3}" font-family="{SANS}" font-size="10" '
                 f'fill="{TXT2}">{esc(txt)}</text>')
        yy += 24

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

def rev(pts):
    return list(reversed(pts))

# journeys: (path pts, f0, f1, color, radius)
JOURNEYS = [
    (PATHS["call1"], 2, 9, MINT, 4),
    (PATHS["query"], 9, 20, MINT, 4),
    (rev(PATHS["dbread"]), 20, 30, MINT, 4),   # engine -> DB (query out)
    (PATHS["dbread"], 30, 40, MINT, 4),        # DB -> engine (rows back)
    (PATHS["bundle"], 44, 62, MINT, 4.5),      # ranked bundle back to agents
    (PATHS["call2"], 8, 15, ORANGE, 4),
    (PATHS["propose"], 15, 27, ORANGE, 4),
    (PATHS["s2g"], 27, 32, ORANGE, 4),
    (PATHS["gate2db"], 67, 78, ORANGE, 4),
    (PATHS["direct"], 84, 96, GRAY, 3.5),
    (PATHS["syncp"], 96, 108, GRAY, 3.5),
]

HOLD_POS = (694, 380)  # proposal waiting inside the gate
HOLD_F0, HOLD_F1 = 32, 67

def frame_state(f):
    dots = []
    for pts, f0, f1, col, r in JOURNEYS:
        if f0 <= f <= f1:
            t = smooth((f - f0) / (f1 - f0))
            x, y = point_at(pts, t)
            # fade in/out over ~1.5 frames
            op = min(1.0, (f - f0) / 1.5, (f1 - f) / 1.5 + 0.34)
            op = max(0.0, min(1.0, op))
            dots.append((x, y, col, r, op))
    gate_pulse = 0.0
    if HOLD_F0 <= f <= HOLD_F1:
        dots.append((HOLD_POS[0], HOLD_POS[1], ORANGE, 4, 1.0))
        gate_pulse = 0.5 + 0.5 * math.sin((f - HOLD_F0) * 0.45)
    gate_flash = 0.0
    if 64 <= f <= 69:
        gate_flash = 1.0 - abs(f - 66.5) / 2.5
    engine_pulse = 0.0
    if 38 <= f <= 46:
        engine_pulse = 1.0 - abs(f - 42) / 4.0
    db_pulse = 0.0
    for c0, c1 in [(27, 34), (76, 84), (93, 99)]:
        if c0 <= f <= c1:
            mid = (c0 + c1) / 2
            db_pulse = max(db_pulse, 1.0 - abs(f - mid) / ((c1 - c0) / 2))
    sync_pulse = 0.0
    if 106 <= f <= 112:
        sync_pulse = 1.0 - abs(f - 109) / 3.0
    return dict(dots=dots, gate_pulse=gate_pulse, gate_flash=gate_flash,
                engine_pulse=engine_pulse, db_pulse=db_pulse, sync_pulse=sync_pulse)

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
