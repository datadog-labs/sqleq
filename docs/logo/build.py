#!/usr/bin/env python3
"""Build the sqleq marks.

Geometry is declared as rects so it can be rasterized exactly and eyeballed without
an SVG renderer (this box has none).

The mark is |- followed by the identity sign: "proves equivalent". The identity sign
is drawn as CELLS rather than solid rules, so it carries two readings at once -- three
horizontal bands (the identity sign) from a distance, a grid of table cells up close.
Cells group tighter horizontally (gap 2) than rows do vertically (gap 3), which is what
keeps the identity reading dominant.

Mono is primary. The axis-coloured variant stays available but is only honest while the
portfolio is exactly qed / sqlsolver / fuzz -- a fourth tool makes it wrong.
"""
import io, os

OUT = os.path.dirname(os.path.abspath(__file__))

QED = ("#3b5bdb", "#93a7fc")      # (on light ground, on dark ground)
SQS = ("#0f766e", "#5cbfb3")
FUZ = ("#b45309", "#e9a23b")
INK = ("#171a21", "#e6e9f0")
MONO_STACK = '"IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace'

def R(x, y, w, h, fill, rx=1):
    return dict(x=x, y=y, w=w, h=h, fill=fill, rx=rx)

STEM, ARM = (12, 19, 5, 26), (17, 30, 12, 4)
BAR_X, CELL, GAP_H, BAR_H = 34, 5, 2, 4
# Cells are WIDER than tall -- square cells make a 3x3 grid, which is the app-launcher
# icon. Row gap 4 is double the column gap 2, so rows group and the identity sign wins.
ROW_Y = (22, 30, 38)

def turnstile(ink, c1, c2, c3, cells=True):
    rs = [R(*STEM, ink), R(*ARM, ink)]
    for y, c in zip(ROW_Y, (c1, c2, c3)):
        if cells:
            rs += [R(BAR_X + i * (CELL + GAP_H), y, CELL, BAR_H, c) for i in range(3)]
        else:
            rs.append(R(BAR_X, y, 3 * CELL + 2 * GAP_H, BAR_H, c))
    return rs

def square(ink, c1, c2, c3, t=4):
    x, y, w, h = 10, 10, 44, 44
    rs = [R(x, y, w, t, ink), R(x, y + h - t, w, t, ink),
          R(x, y + t, t, h - 2 * t, ink), R(x + w - t, y + t, t, h - 2 * t, ink)]
    for yy, c in zip(ROW_Y, (c1, c2, c3)):
        rs += [R(22.5 + i * (CELL + GAP_H), yy, CELL, BAR_H, c) for i in range(3)]
    return rs

# Pixel-snapped favicon, designed ON a 16-unit grid. Bars stay SOLID here: cells would
# need 2px wide with 1px gaps, which reads as noise rather than as a table.
def favicon(ink, c1, c2, c3):
    rs = [R(0, 0, 16, 1, ink, 0), R(0, 15, 16, 1, ink, 0),
          R(0, 1, 1, 14, ink, 0), R(15, 1, 1, 14, ink, 0)]
    return rs + [R(4, 3 + 4 * i, 8, 2, c, 0) for i, c in enumerate((c1, c2, c3))]

def bbox(rs):
    return (min(r["x"] for r in rs), max(r["x"] + r["w"] for r in rs),
            min(r["y"] for r in rs), max(r["y"] + r["h"] for r in rs))

def fit(rs, margin=4, square_box=False):
    x0, x1, y0, y1 = bbox(rs)
    w, h = x1 - x0 + 2 * margin, y1 - y0 + 2 * margin
    dx, dy = margin - x0, margin - y0
    if square_box:
        s = max(w, h); dx += (s - w) / 2; dy += (s - h) / 2; w = h = s
    return [dict(r, x=r["x"] + dx, y=r["y"] + dy) for r in rs], round(w, 2), round(h, 2)

def body(rs):
    return "\n".join('  <rect x="%g" y="%g" width="%g" height="%g" rx="%g" fill="%s"/>'
                     % (r["x"], r["y"], r["w"], r["h"], r["rx"], r["fill"]) for r in rs)

def svg(rs, vw, vh, desc):
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %g %g" width="%g" height="%g" '
            'role="img" aria-labelledby="t d">\n  <title id="t">sqleq</title>\n'
            '  <desc id="d">%s</desc>\n%s\n</svg>\n' % (vw, vh, vw, vh, desc, body(rs)))

def lockup(ink, c1, c2, c3):
    rs, vw, vh = fit(turnstile(ink, c1, c2, c3), margin=3)
    # No renderer and no Plex on this box: extent is computed from Plex Mono's 0.6em
    # advance, not measured. Outline the text for production.
    W = round(vw + 8 + (5 * 0.6 * 23 - 4 * 0.6) + 3, 1)
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %g %g" width="%g" height="%g" '
            'role="img" aria-label="sqleq">\n%s\n  <text x="%g" y="%g" font-family=%s '
            'font-size="23" font-weight="600" letter-spacing="-0.6" fill="%s" '
            'dominant-baseline="central">sqleq</text>\n</svg>\n'
            % (W, vh, W, vh, body(rs), vw + 8, vh / 2 + 0.5, "'%s'" % MONO_STACK, ink))

D_MARK = ("The entailment turnstile followed by the identity sign. The identity sign is built "
          "from table cells, so it reads as both 'equivalent' and 'a table'.")
D_SQ = "The end-of-proof square enclosing the identity sign. For avatars and square frames."
D_FAV = "Pixel-snapped favicon; every edge lands on a device pixel at 16, 32 and 64px."

VARIANTS = {}
for i, tag in ((0, "light"), (1, "dark")):
    k, ax = [INK[i]] * 4, (INK[i], QED[i], SQS[i], FUZ[i])
    VARIANTS["mark-%s" % tag]         = (fit(turnstile(*k)), D_MARK)
    VARIANTS["mark-axes-%s" % tag]    = (fit(turnstile(*ax)), D_MARK + " Axis-coloured variant.")
    VARIANTS["square-%s" % tag]       = (fit(square(*k), margin=5, square_box=True), D_SQ)
    VARIANTS["square-axes-%s" % tag]  = (fit(square(*ax), margin=5, square_box=True), D_SQ)
    VARIANTS["favicon-%s" % tag]      = ((favicon(*k), 16, 16), D_FAV)

def raster(rs, vw, vh, cols=64, square_px=False):
    rows = cols if square_px else max(1, int(round(cols * vh / vw / 2)))
    keys = list(dict.fromkeys(r["fill"] for r in rs))
    sym = {k: "#@*+=~"[i % 6] for i, k in enumerate(keys)}
    out = []
    for cy in range(rows):
        row = ""
        for cx in range(cols):
            px, py, c = (cx + .5) * vw / cols, (cy + .5) * vh / rows, " "
            for r in rs:
                if r["x"] <= px < r["x"] + r["w"] and r["y"] <= py < r["y"] + r["h"]:
                    c = sym[r["fill"]]
            row += c
        out.append(row.rstrip())
    return "\n".join(out), sym

if __name__ == "__main__":
    for name, ((rs, vw, vh), desc) in VARIANTS.items():
        io.open(os.path.join(OUT, name + ".svg"), "w", encoding="utf-8").write(svg(rs, vw, vh, desc))
    for tag, i in (("light", 0), ("dark", 1)):
        io.open(os.path.join(OUT, "lockup-%s.svg" % tag), "w", encoding="utf-8").write(
            lockup(*[INK[i]] * 4))
    print("wrote %d svg files" % (len(VARIANTS) + 2))
