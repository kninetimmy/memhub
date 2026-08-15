#!/usr/bin/env python3
"""memhub cross-machine sync diagram, Style 8 Dark Luxury, hand-built topology.

Static SVG + per-frame SVGs with computed dot positions (no SMIL, no tails).
Usage:
  python sync-model-motion-gen.py static out.svg
  python sync-model-motion-gen.py frames outdir/   # frame_000.svg .. frame_114.svg
"""
import math
import sys
import os

W, H = 960, 520
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

GATE_X = 664  # confirm gate on the adopt corridor

PATHS = {
    "push":    [(256, 172), (330, 172)],
    "check":   [(630, 172), (704, 172)],
    "adopt1":  [(630, 236), (GATE_X - 6, 236)],
    "adopt2":  [(GATE_X - 6, 236), (704, 236)],
}

EDGES = [
    # name, color, width, dash, marker_end
    ("push",  MINT,   1.5, None, "arr-mint"),
    ("check", SKY,    1.5, None, "arr-sky"),
    ("adopt1", ORANGE, 1.5, None, None),
    ("adopt2", ORANGE, 1.5, None, "arr-orange"),
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

def chip(L, x, y, w, h, stroke, name, fill_col, size=12, dash=None):
    dd = f' stroke-dasharray="{dash}"' if dash else ""
    L.append(f'  <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="5" '
             f'fill="{SURF}" stroke="{stroke}" stroke-width="1.25"{dd}/>')
    L.append(f'  <text x="{x+w/2}" y="{y+h/2+4}" text-anchor="middle" '
             f'font-family="{MONO}" font-size="{size}" font-weight="600" '
             f'fill="{fill_col}">{esc(name)}</text>')

def pulse_rect(L, x, y, w, h, color, op, width=1.5, rx=7):
    L.append(f'  <rect x="{x-3}" y="{y-3}" width="{w+6}" height="{h+6}" rx="{rx}" '
             f'fill="none" stroke="{color}" stroke-width="{width}" opacity="{op:.2f}"/>')

def machine(L, x, label, caption, marker_pulse=0.0, db_pulse=0.0):
    L.append(f'  <rect x="{x}" y="110" width="220" height="170" rx="8" fill="{SURF}" '
             f'stroke="{GOLD}" stroke-width="0.5" stroke-dasharray="6,4" opacity="0.9"/>')
    L.append(f'  <text x="{x+12}" y="132" font-family="{SERIF}" font-size="11" '
             f'font-weight="700" fill="{GOLD_DIM}" opacity="0.75">{esc(label)}</text>')
    chip(L, x + 16, 146, 188, 30, SKY, "project.sqlite", SKY)
    if db_pulse > 0:
        pulse_rect(L, x + 16, 146, 188, 30, SKY, 0.6 * db_pulse, rx=6)
    chip(L, x + 16, 186, 188, 26, GRAY, "sync_marker.json", TXT2, size=10)
    if marker_pulse > 0:
        pulse_rect(L, x + 16, 186, 188, 26, SKY, 0.6 * marker_pulse, rx=6)
    L.append(f'  <text x="{x+110}" y="230" text-anchor="middle" font-family="{SANS}" '
             f'font-size="9" fill="{TXT3}">last-synced baseline</text>')
    L.append(f'  <text x="{x+110}" y="260" text-anchor="middle" font-family="{SANS}" '
             f'font-size="10.5" fill="{TXT2}">{esc(caption)}</text>')

def build_svg(dots=None, snap_pulse=0.0, manifest_flash=0.0, folder_pulse=0.0,
              a_db_pulse=0.0, b_marker_pulse=0.0, b_db_pulse=0.0,
              backup_pulse=0.0, gate_pulse=0.0, gate_flash=0.0,
              verdict_pulse=None):
    verdict_pulse = verdict_pulse or {}
    L = []
    L.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}">')
    L.append('  <defs>')
    for mid, col in [("arr-mint", MINT), ("arr-sky", SKY), ("arr-orange", ORANGE)]:
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

    # title (top-left)
    L.append(f'  <text x="36" y="46" font-family="{SERIF}" font-size="22" '
             f'font-weight="700" fill="{TXT}">cross-machine sync</text>')
    L.append(f'  <text x="36" y="64" font-family="{SANS}" font-size="10.5" '
             f'fill="{TXT2}">a folder that already syncs is the transport - '
             f'memhub stays offline</text>')

    # arrows (before boxes so boxes sit on top of endpoints)
    for name, col, wd, dash, mend in EDGES:
        d = path_d(PATHS[name])
        attrs = f'fill="none" stroke="{col}" stroke-width="{wd}"'
        if dash:
            attrs += f' stroke-dasharray="{dash}"'
        if mend:
            attrs += f' marker-end="url(#{mend})"'
        L.append(f'  <path d="{d}" {attrs} data-graph-role="edge"/>')

    def albl(x, y, text, mono=False, anchor="middle", size=10, col="#8c7e72"):
        fam = MONO if mono else SANS
        L.append(f'  <text x="{x}" y="{y}" text-anchor="{anchor}" font-family="{fam}" '
                 f'font-size="{size}" fill="{col}">{esc(text)}</text>')

    albl(293, 160, "snapshot", size=10)
    albl(293, 186, "VACUUM INTO", mono=True, size=8)
    albl(667, 160, "check", size=10)
    albl(667, 186, "compare digests", size=8)
    albl(645, 225, "adopt", size=10)

    # confirm gate on the adopt corridor
    L.append(f'  <line x1="{GATE_X}" y1="222" x2="{GATE_X}" y2="250" '
             f'stroke="{GOLD}" stroke-width="1.5" stroke-dasharray="3,3"/>')
    if gate_pulse > 0:
        L.append(f'  <line x1="{GATE_X}" y1="218" x2="{GATE_X}" y2="254" '
                 f'stroke="{GOLD_BRIGHT}" stroke-width="2.5" '
                 f'opacity="{(0.25 + 0.5 * gate_pulse):.2f}"/>')
    if gate_flash > 0:
        L.append(f'  <circle cx="{GATE_X}" cy="236" r="14" fill="{GOLD_BRIGHT}" '
                 f'opacity="{0.3*gate_flash:.2f}"/>')
    albl(GATE_X, 266, "you confirm", size=9, col=GOLD_DIM)

    # machines
    machine(L, 36, "MACHINE A", "pushes at /wrap-up",
            db_pulse=a_db_pulse)
    machine(L, 704, "MACHINE B", "pulls at /catch-up",
            marker_pulse=b_marker_pulse, db_pulse=b_db_pulse)

    # backup chip under machine B
    chip(L, 744, 290, 150, 22, GRAY, ".memhub/backups/sync/", TXT3, size=9,
         dash="4,3")
    if backup_pulse > 0:
        pulse_rect(L, 744, 290, 150, 22, GRAY, 0.6 * backup_pulse, rx=6)
    albl(736, 305, "backup first", size=9, anchor="end")

    # synced folder
    L.append(f'  <rect x="330" y="96" width="300" height="220" rx="8" fill="{SURF}" '
             f'stroke="{GRAY}" stroke-width="1" stroke-dasharray="6,4" opacity="0.9"/>')
    if folder_pulse > 0:
        pulse_rect(L, 330, 96, 300, 220, GRAY, 0.5 * folder_pulse)
    L.append(f'  <text x="480" y="118" text-anchor="middle" font-family="{SERIF}" '
             f'font-size="11" font-weight="700" fill="{TXT2}">SYNCED FOLDER - '
             f'DRIVE / RCLONE</text>')
    L.append(f'  <text x="480" y="134" text-anchor="middle" font-family="{MONO}" '
             f'font-size="9" fill="{TXT3}">{esc("<drive_subpath>/memhub/<project_id>")}</text>')
    chip(L, 346, 150, 268, 44, SKY, "project-<sha256>.sqlite", SKY)
    L.append(f'  <text x="480" y="186" text-anchor="middle" font-family="{SANS}" '
             f'font-size="9" fill="{TXT2}">content-addressed - immutable</text>')
    if snap_pulse > 0:
        pulse_rect(L, 346, 150, 268, 44, SKY, 0.6 * snap_pulse, rx=6)
    chip(L, 346, 206, 268, 44, AMBER, "manifest.json", AMBER)
    L.append(f'  <text x="480" y="242" text-anchor="middle" font-family="{SANS}" '
             f'font-size="9" fill="{TXT2}">written last - the publish moment</text>')
    if manifest_flash > 0:
        L.append(f'  <rect x="346" y="206" width="268" height="44" rx="5" '
                 f'fill="{AMBER}" opacity="{0.25*manifest_flash:.2f}"/>')
    L.append(f'  <text x="480" y="290" text-anchor="middle" font-family="{SANS}" '
             f'font-size="9" fill="{TXT3}">snapshot first, manifest last - '
             f'a half-written push is never visible</text>')

    # verdict strip
    albl(480, 336, "every sync check ends in one of five verdicts", size=9)
    verdicts = [
        ("no-remote", GRAY, "nothing pushed yet"),
        ("up-to-date", MINT, "in sync - nothing to do"),
        ("local-ahead", SKY, "this machine is newer - push"),
        ("drive-ahead", VIOLET, "the folder is newer - pull"),
        ("diverged", ORANGE, "both changed - you choose"),
    ]
    x = 28
    for name, col, sub in verdicts:
        L.append(f'  <rect x="{x}" y="346" width="168" height="42" rx="7" '
                 f'fill="{SURF}" stroke="{col}" stroke-width="1" opacity="0.9"/>')
        vp = verdict_pulse.get(name, 0.0)
        if vp > 0:
            pulse_rect(L, x, 346, 168, 42, col, 0.6 * vp)
        L.append(f'  <text x="{x+84}" y="{346+18}" text-anchor="middle" '
                 f'font-family="{MONO}" font-size="11" font-weight="600" '
                 f'fill="{col}">{esc(name)}</text>')
        L.append(f'  <text x="{x+84}" y="{346+33}" text-anchor="middle" '
                 f'font-family="{SANS}" font-size="8" fill="{TXT2}">{esc(sub)}</text>')
        x += 184

    # offline banner
    L.append(f'  <rect x="28" y="412" width="904" height="46" rx="8" fill="{SURF}" '
             f'stroke="{GOLD}" stroke-width="1" opacity="0.95"/>')
    L.append(f'  <text x="480" y="431" text-anchor="middle" font-family="{SANS}" '
             f'font-size="12.5" font-weight="700" fill="{GOLD_BRIGHT}">memhub only '
             f'reads and writes a local path</text>')
    L.append(f'  <text x="480" y="448" text-anchor="middle" font-family="{SANS}" '
             f'font-size="10" fill="{TXT2}">Drive for Desktop or rclone moves the '
             f'bytes - memhub never makes a network call</text>')

    # legend (one row)
    entries = [
        (MINT, "push (snapshot)"),
        (SKY, "check (read-only)"),
        (ORANGE, "adopt - gated"),
        (GOLD, "your confirm"),
    ]
    x = 120
    for col, txt in entries:
        L.append(f'  <line x1="{x}" y1="492" x2="{x+28}" y2="492" stroke="{col}" '
                 f'stroke-width="1.5"/>')
        L.append(f'  <text x="{x+36}" y="495" font-family="{SANS}" font-size="10" '
                 f'fill="{TXT2}">{esc(txt)}</text>')
        x += 190

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
    (PATHS["push"], 4, 12, MINT, 4),
    (PATHS["check"], 38, 46, SKY, 4),
    (PATHS["adopt1"], 54, 60, ORANGE, 4),
    (PATHS["adopt2"], 80, 86, ORANGE, 4),
]

HOLD_POS = (GATE_X - 6, 236)  # adopt waiting at the confirm gate
HOLD_F0, HOLD_F1 = 60, 80

def tri(f, c0, c1):
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
    gate_pulse = 0.0
    if HOLD_F0 <= f <= HOLD_F1:
        dots.append((HOLD_POS[0], HOLD_POS[1], ORANGE, 4, 1.0))
        gate_pulse = 0.5 + 0.5 * math.sin((f - HOLD_F0) * 0.45)
    gate_flash = tri(f, 77, 82)
    verdict_pulse = {
        "local-ahead": tri(f, 2, 16),
        "drive-ahead": tri(f, 44, 56),
        "up-to-date": tri(f, 98, 110),
    }
    return dict(dots=dots,
                a_db_pulse=tri(f, 2, 8),
                snap_pulse=tri(f, 11, 19),
                manifest_flash=tri(f, 19, 26),
                folder_pulse=tri(f, 26, 36),
                b_marker_pulse=tri(f, 44, 52),
                gate_pulse=gate_pulse,
                gate_flash=gate_flash,
                backup_pulse=tri(f, 84, 92),
                b_db_pulse=tri(f, 88, 98),
                verdict_pulse=verdict_pulse)

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
