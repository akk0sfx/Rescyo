#!/usr/bin/env python3
"""Generates Rescyo's brand mark and app icon as SVG.

The mark is a rescue ring that is also a disk platter cut into sectors. Three segments are in
place; the top-right one, in rescue orange, is returning to its slot along the diagonal, whose
dashed outline still shows where it belongs. Geometry is
computed here so every export (README logo, app icons, Icon Composer layers) stays identical.

Usage: python3 build_brand.py <out-dir>
"""

import math
import sys
from pathlib import Path

C = 512.0                 # canvas centre (1024 x 1024)
R_OUT, R_IN = 262.0, 140.0
GAP = 26.0                # constant-width gap between segments
RC_OUT, RC_IN = 18.0, 12.0
LIFT = 30.0               # how far the rescued segment still is from its slot

ORANGE_HI, ORANGE_LO = "#FF8A3D", "#FF3B1F"
INK_HI, INK_LO = "#23242F", "#0A0A0F"
PAPER_HI, PAPER_LO = "#FFFFFF", "#D9DAE6"


def pt(r, a, dx=0.0, dy=0.0):
    return (C + dx + r * math.cos(a), C + dy + r * math.sin(a))


def f(p):
    return f"{p[0]:.2f} {p[1]:.2f}"


def segment(a1, a2, dx=0.0, dy=0.0):
    """Annular sector between angles a1 < a2 (radians, screen coordinates), with edges offset
    by GAP/2 so gaps keep a constant width, and filleted corners."""
    h = GAP / 2

    def corner(a, side, outer):
        # side = +1 for the edge at a1 (fillet towards larger angles), -1 for the edge at a2.
        rc = RC_OUT if outer else RC_IN
        d = (R_OUT - rc) if outer else (R_IN + rc)
        da = math.asin((h + rc) / d)
        centre_a = a + side * da
        on_circle = pt(R_OUT if outer else R_IN, centre_a, dx, dy)
        along = math.sqrt(d * d - (h + rc) ** 2)
        # Point on the offset edge line: along the radial direction, shifted by h sideways.
        nx, ny = -math.sin(a) * side, math.cos(a) * side
        on_line = (C + dx + along * math.cos(a) + h * nx, C + dy + along * math.sin(a) + h * ny)
        return on_line, on_circle, rc

    l1o, c1o, rco = corner(a1, +1, True)
    l2o, c2o, _ = corner(a2, -1, True)
    l2i, c2i, rci = corner(a2, -1, False)
    l1i, c1i, _ = corner(a1, +1, False)
    large = 1 if (a2 - a1) > math.pi else 0
    return (
        f"M {f(l1o)} "
        f"A {rco} {rco} 0 0 1 {f(c1o)} "
        f"A {R_OUT} {R_OUT} 0 {large} 1 {f(c2o)} "
        f"A {rco} {rco} 0 0 1 {f(l2o)} "
        f"L {f(l2i)} "
        f"A {rci} {rci} 0 0 1 {f(c2i)} "
        f"A {R_IN} {R_IN} 0 {large} 0 {f(c1i)} "
        f"A {rci} {rci} 0 0 1 {f(l1i)} Z"
    )


def squircle(x0, y0, size, n=5.0, steps=256):
    """Superellipse body like the macOS icon grid (continuous corners)."""
    cx, cy, r = x0 + size / 2, y0 + size / 2, size / 2
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        ct, st = math.cos(t), math.sin(t)
        x = cx + r * math.copysign(abs(ct) ** (2 / n), ct)
        y = cy + r * math.copysign(abs(st) ** (2 / n), st)
        pts.append((x, y))
    return "M " + " L ".join(f(p) for p in pts) + " Z"


Q = math.pi / 2
# The whole mark is turned 45 degrees clockwise: segments centred on the diagonals, gaps
# straight up, down, left and right. The rescued one is the top-right segment, lifted outwards
# along its diagonal.
ROTATION = Q / 2
SLOTS = [(a + ROTATION, b + ROTATION) for a, b in [(-1.5 * Q, -0.5 * Q), (-0.5 * Q, 0.5 * Q), (0.5 * Q, 1.5 * Q), (1.5 * Q, 2.5 * Q)]]
RESCUED = 0
LIFT_ANGLE = -Q + ROTATION
LIFT_DX = LIFT * math.cos(LIFT_ANGLE)
LIFT_DY = LIFT * math.sin(LIFT_ANGLE)


def defs():
    return f"""
  <defs>
    <linearGradient id="ink" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{INK_HI}"/><stop offset="1" stop-color="{INK_LO}"/>
    </linearGradient>
    <linearGradient id="paper" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{PAPER_HI}"/><stop offset="1" stop-color="{PAPER_LO}"/>
    </linearGradient>
    <linearGradient id="orange" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{ORANGE_HI}"/><stop offset="1" stop-color="{ORANGE_LO}"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.5" r="0.5">
      <stop offset="0" stop-color="#FF5A1F" stop-opacity="0.55"/>
      <stop offset="1" stop-color="#FF5A1F" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#FFFFFF" stop-opacity="0.22"/>
      <stop offset="0.35" stop-color="#FFFFFF" stop-opacity="0.04"/>
      <stop offset="1" stop-color="#FFFFFF" stop-opacity="0.02"/>
    </linearGradient>
  </defs>"""


def mark(paper="url(#paper)", ghost="#FFFFFF", glow=True):
    a1, a2 = SLOTS[RESCUED]
    parts = []
    if glow:
        gx, gy = pt((R_IN + R_OUT) / 2, LIFT_ANGLE, LIFT_DX, LIFT_DY)
        parts.append(f'<circle cx="{gx:.1f}" cy="{gy:.1f}" r="230" fill="url(#glow)"/>')
    parts.append(f'<path d="{segment(a1, a2)}" fill="{ghost}" fill-opacity="0.07" '
                 f'stroke="{ghost}" stroke-opacity="0.16" stroke-width="3" stroke-dasharray="10 12"/>')
    for i, (b1, b2) in enumerate(SLOTS):
        if i != RESCUED:
            parts.append(f'<path d="{segment(b1, b2)}" fill="{paper}"/>')
    parts.append(f'<path d="{segment(a1, a2, LIFT_DX, LIFT_DY)}" fill="url(#orange)"/>')
    return "\n  ".join(parts)


def svg(body, view="0 0 1024 1024", w=1024):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{w}" viewBox="{view}">'
            f'{defs()}\n  {body}\n</svg>\n')


# The lifted segment adds weight towards it: shift the mark back by half the lift to centre it.
OPTICAL = (
    f'transform="translate({-LIFT / 2 * math.cos(LIFT_ANGLE):.1f} {-LIFT / 2 * math.sin(LIFT_ANGLE):.1f})"'
)


def icon_body():
    body = squircle(100, 100, 824)
    return (f'<clipPath id="body"><path d="{body}"/></clipPath>\n  '
            f'<path d="{body}" fill="url(#ink)"/>\n  '
            f'<g clip-path="url(#body)"><g {OPTICAL}>{mark()}</g></g>\n  '
            f'<path d="{body}" fill="none" stroke="url(#rim)" stroke-width="4"/>')


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    out.mkdir(parents=True, exist_ok=True)
    # macOS-grid app icon: 824 body on a 1024 canvas.
    (out / "app-icon.svg").write_text(svg(icon_body()))
    # README / web logo: the same icon cropped to its body.
    (out / "logo.svg").write_text(svg(icon_body(), view="96 96 832 832", w=256))
    # Mark only, for dark and light backgrounds.
    (out / "mark-dark.svg").write_text(svg(f"<g {OPTICAL}>{mark(glow=False)}</g>", view="200 200 624 624", w=256))
    (out / "mark-light.svg").write_text(
        svg(f'<g {OPTICAL}>{mark(paper="#16171F", ghost="#16171F", glow=False)}</g>',
            view="200 200 624 624", w=256))
    # Icon Composer layers (macOS 26 Liquid Glass): background and foreground separately.
    layers = out / "icon-layers"
    layers.mkdir(exist_ok=True)
    (layers / "0-background.svg").write_text(svg('<rect width="1024" height="1024" fill="url(#ink)"/>'))
    ring = "\n  ".join(f'<path d="{segment(b1, b2)}" fill="#FFFFFF"/>'
                       for i, (b1, b2) in enumerate(SLOTS) if i != RESCUED)
    (layers / "1-ring.svg").write_text(svg(ring))
    a1, a2 = SLOTS[RESCUED]
    (layers / "2-rescued.svg").write_text(svg(
        f'<path d="{segment(a1, a2, LIFT_DX, LIFT_DY)}" fill="url(#orange)"/>'))


if __name__ == "__main__":
    main()
