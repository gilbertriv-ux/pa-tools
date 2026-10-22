#!/usr/bin/env python3
"""Pa Tools app icon: writes the original vector artwork.

    python3 packaging/icon_svg.py assets/app-icon/icon.svg
    packaging/icons.sh                      # then re-render every size

Pure geometry, no fonts, no tracing, no third-party artwork: a gold "Pa" built from two rings and two
bars (the P bowl and the a ring share one stroke weight), an ivory waveform of 15 round-capped bars
under it, and "TOOLS" drawn from strokes. Three colours only. Keep viewBox="0 0 512 512", because
icons.sh widens it for the macOS grid.
"""
import sys
GOLD, INK, SILVER = "#C8A24B", "#0b0b0c", "#D8D4C8"

S = 46            # wordmark stroke
RM = 45           # ring mid-radius (outer radius 68, inner 22)
TOP, BASE = 82, 298
CXP, CXA = 176, 336            # ring centres of "P" and "a"
CYP = TOP + RM + S // 2        # P bowl centre y = 150
CYA = BASE - RM - S // 2       # a ring centre y = 230
XH = CYA - (RM + S // 2)       # x-height line = 162

wordmark = (
    f'<path d="M{CXP - RM} {TOP}V{BASE}"/>'                       # P stem (left band of its bowl)
    f'<circle cx="{CXP}" cy="{CYP}" r="{RM}"/>'                    # P bowl
    f'<circle cx="{CXA}" cy="{CYA}" r="{RM}"/>'                    # a ring
    f'<path d="M{CXA + RM} {XH}V{BASE}"/>'                         # a stem (right band of its ring)
)

# waveform: 15 round-capped bars centred on y=350; visible heights include the caps
H = [16, 28, 20, 44, 30, 60, 34, 52, 24, 56, 30, 42, 18, 28, 14]
x0, x1 = CXP - 68 + 6, CXA + 68 - 6
pitch = (x1 - x0) / (len(H) - 1)
CY = 350
bars = "".join(f"M{x0 + k * pitch:.1f} {CY - (h - 12) / 2:g}v{h - 12}" for k, h in enumerate(H))

# "TOOLS": stroke-built letters, cap height 28, top y=400, stroke 6, centred on x=256
T0, TH, TS = 400, 28, 6
widths, gap = [24, 24, 24, 20, 22], 16
xs, x = [], 256 - (sum(widths) + gap * 4) / 2
for w in widths:
    xs.append(x)
    x += w + gap
t_x, o1_x, o2_x, l_x, s_x = xs
h2 = TS / 2
T = f"M{t_x:g} {T0 + h2:g}h{widths[0]}M{t_x + widths[0] / 2:g} {T0 + h2:g}V{T0 + TH}"
L = f"M{l_x + h2:g} {T0}V{T0 + TH - h2:g}H{l_x + widths[3]:g}"
O = lambda ox: f'<rect x="{ox + h2:g}" y="{T0 + h2:g}" width="{widths[1] - TS}" height="{TH - TS}" rx="{(widths[1] - TS) / 2:g}"/>'
# S in a 22 x 28 box, inset by half the stroke
sx, sy = s_x + h2, T0 + h2
Sp = (f"M{sx + 16:g} {sy + 5:g}C{sx + 16:g} {sy - 1.5:g} {sx:g} {sy - 1.5:g} {sx:g} {sy + 5:g}"
      f"C{sx:g} {sy + 10:g} {sx + 16:g} {sy + 12:g} {sx + 16:g} {sy + 17:g}"
      f"C{sx + 16:g} {sy + 23.5:g} {sx:g} {sy + 23.5:g} {sx:g} {sy + 17:g}")

svg = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><title>Pa Tools</title>'
    '<defs><clipPath id="t"><rect width="512" height="512" rx="112"/></clipPath></defs>'
    f'<g clip-path="url(#t)"><rect width="512" height="512" fill="{INK}"/>'
    f'<g fill="none" stroke="{GOLD}" stroke-width="{S}">{wordmark}</g>'
    f'<path fill="none" stroke="{SILVER}" stroke-width="12" stroke-linecap="round" d="{bars}"/>'
    f'<g fill="none" stroke="{SILVER}" stroke-width="{TS}"><path d="{T}{L}"/>{O(o1_x)}{O(o2_x)}'
    f'<path stroke-linecap="round" d="{Sp}"/></g></g></svg>\n'
)
open(sys.argv[1], "w").write(svg)
