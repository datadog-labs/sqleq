<picture>
  <source media="(prefers-color-scheme: dark)" srcset="mark-dark.svg">
  <img src="mark-light.svg" alt="" width="96">
</picture>

# Logo

The entailment turnstile followed by the identity sign: **proves equivalent** — the judgement
this project makes, written in the notation it would be written in.

Two things are load-bearing rather than decorative:

- **The turnstile.** A bare `≡` is the hamburger-menu icon, and would be read as one everywhere
  it appeared.
- **The cells.** The identity sign is built from table cells instead of solid rules, so the same
  three bars read as three bands at a glance and as a grid of table cells up close. That is where
  the SQL is. Nothing was added to the mark to get it.

Two ratios keep the identity reading dominant, and both matter if you ever redraw this. Row gaps
are **double** the column gaps and run the full width uninterrupted, so rows group before columns
do. And cells are **wider than they are tall** — square cells produce a 3×3 grid, which is the
app-launcher icon.

## Which file

| file | use |
|---|---|
| `mark-{light,dark}.svg` | **primary.** Horizontal, 49×34. READMEs, headers, docs, slides |
| `square-{light,dark}.svg` | the end-of-proof square, 54×54. Avatars, square frames |
| `favicon-{light,dark}.svg` | 16×16, pixel-snapped. **Use below 32px** |
| `lockup-{light,dark}.svg` | mark + wordmark. Read the caveat below before using it |
| `mark-axes-*`, `square-axes-*` | axis-coloured. Conditional — see below |

Pick `-light` for light grounds and `-dark` for dark ones; in Markdown, `<picture>` with a
`prefers-color-scheme` source does it automatically, as at the top of this file.

`index.html` is a contact sheet showing every variant at 16/24/32/48/96px on both grounds. Open it
in a browser — there is no SVG renderer in this repo's toolchain, so it is the review surface.

## Three rules

1. **Below 32px use the favicon file**, not a scaled-down square mark. The square mark's bars fall
   to 1.5px with a 0.9px gap at 16px, and antialiasing merges them into a grey block. The favicon
   is drawn *on* a 16-unit grid — 2px bars, 2px gaps, 1px frame, every coordinate an integer — and
   its bars are deliberately solid, because cells there would be 2px wide with 1px gaps and read
   as noise.
2. **Keep the turnstile.** Without it the mark is a hamburger icon.
3. **The axis-coloured variant has an expiry date.** Colouring the three bars for `qed`,
   `sqlsolver` and `fuzz` turns the mark into a legend for the portfolio, so a fourth tool makes
   it *wrong*, not merely dated. The single-colour mark carries no count. Use the coloured one
   only where the three axes are themselves the subject. Its hues are lifted from the generated
   reports; nothing was invented for the logo.

## Caveat: the wordmark is live text

`lockup-*.svg` sets `sqleq` as an SVG `<text>` element in IBM Plex Mono. Anywhere that font is
missing — including GitHub, which renders SVG in an `<img>` context with no webfont — the browser
substitutes and the spacing changes. Its `viewBox` width is *computed* from Plex Mono's 0.6em
advance rather than measured, for the same reason. **Outline the text before using the lockup
anywhere it matters**; that settles the width too. The repo README uses the mark, not the lockup,
precisely because the mark is pure geometry and renders identically everywhere.

## Regenerating

```sh
python3 build.py    # every .svg in this directory
python3 sheet.py    # index.html
```

Both are idempotent: `rm -f *.svg index.html && python3 build.py && python3 sheet.py` reproduces
the set exactly. Every mark is plain `<rect>` on a half-unit grid — no paths, strokes or
transforms — so they scale without hinting and recolour by editing one hex value in `build.py`.

`build.py` also carries an exact rect rasterizer, which is how the geometry was checked without a
renderer. Two traps it exists to avoid: ASCII character cells are 2:1, which stretches horizontal
gaps and *understates* the identity reading, so judge grouping only with an aspect-correct
square-pixel raster; and nearest-neighbour sampling flatters sub-pixel detail, so compute the real
pixel dimensions rather than trusting the picture.
