# Pa Tools app icon

A black rounded square with a gold **Pa** (a P and an a built from two rings and two bars), an ivory
waveform of fifteen round bars beneath it, and the word **TOOLS** in small ivory capitals under the
waveform. It reads as a studio plate: dark, warm metal, signal.

The artwork is original. It is plain geometry (circles, straight strokes and one Bezier curve for the
S) written by `packaging/icon_svg.py`; no font, no existing logo and no stock image was used or traced.

## Palette

Exactly three colours:

| Colour | Hex | Used for |
|---|---|---|
| Black | `#0b0b0c` | The full-bleed background field |
| Gold | `#C8A24B` | The "Pa" wordmark |
| Ivory | `#D8D4C8` | The waveform and the word TOOLS |

## Geometry

A 512 x 512 viewBox, clipped to a rounded square with `rx=112`. Windows and Linux use the full tile.
The macOS icons (`Pa Tools.icns`, `patools-macos-512.png`) sit on Apple's grid: an 824/1024 body with a
transparent margin (`packaging/icons.sh` widens the viewBox to do this).

The wordmark strokes are 46 units wide and the waveform bars 12, so both survive at 16 px: at that size
the gold "Pa" and the ivory waveform stay distinct shapes, and the small TOOLS lettering (6-unit
strokes) fades into the waveform's texture instead of turning into noise. Do not thicken or move the
lettering to chase legibility at 16 px; it is meant to be read from 64 px up.

## Files

| File | What it is |
|---|---|
| `icon.svg` | The canonical vector (written by `packaging/icon_svg.py`) |
| `patools-1024.png` | 1024 px render |
| `patools-macos-512.png` | Runtime window/Dock icon on macOS (Apple grid margin) |
| `Pa Tools.iconset/` | The ten PNGs `iconutil` reads (16 to 512 px, 1x and 2x), on Apple's grid |
| `Pa Tools.icns` | macOS bundle icon |
| `patools.ico` | Windows icon (16 to 256 px, PNG entries), embedded in `pa-tools.exe` and used by the MSI |
| `hicolor/<size>x<size>/apps/com.imagescouture.patools.png` | Linux/FreeBSD icon theme, 16 to 512 px, plus `scalable/` |

## Regenerating

```sh
python3 packaging/icon_svg.py assets/app-icon/icon.svg   # only if you change the artwork
packaging/icons.sh                                       # re-render every file above
```

`icons.sh` needs `resvg` (`brew install resvg` or `cargo install resvg`) and `python3`. The `.ico` is
packed by `packaging/icons.py`; `ICO_PACKER=xtask packaging/icons.sh` uses `cargo xtask ico` instead
(same layout). The `.icns` comes from macOS' `iconutil` when it is installed, otherwise from
`packaging/icons.py`, which writes the same PNG entry types (`icp4`, `icp5`, `ic07` to `ic14`).

**To do on a Mac:** the committed `Pa Tools.icns` was packed on Linux, and it loads the same way. For
the canonical file, run once:

```sh
iconutil -c icns "assets/app-icon/Pa Tools.iconset"
```

Don't hand-edit the generated files; change `icon_svg.py` (or `icon.svg`) and regenerate.

## Provenance and licence

Original work by GR8 ID YUZ.AI (Images Couture) for Pa Tools, made for this project, MIT OR Apache-2.0
like the code (see `ATTRIBUTION.md`). The previous SoundCraft icon, an engraved nightingale, and
every ArtCraft mark were removed; none of their artwork is used here.
