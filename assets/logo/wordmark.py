"""The appzones wordmark: a pixel font, "zones" in the zone border colours.

    python3 assets/logo/wordmark.py    # writes appzones-{dark,light}.svg and social-preview.svg here
    rsvg-convert assets/logo/social-preview.svg -o assets/logo/social-preview.png

social-preview.png is the repository's social preview (Settings -> Social
preview: 1280x640, a PNG with its own background).

Grid: x-height rows 3-9, descenders rows 10-11, 2-pixel strokes, one empty
column between letters, a 2x2 dot on the baseline.
"""
from pathlib import Path

GLYPHS = {  # rows 0..11, '#' = a pixel
    "a": ["", "", "", "#####.", "######", "....##", ".#####", "##..##", "######", ".#####", "", ""],
    "p": ["", "", "", "#####.", "######", "##..##", "##..##", "##..##", "######", "#####.", "##", "##"],
    "z": ["", "", "", "######", "######", "...##.", "..##..", ".##...", "######", "######", "", ""],
    "o": ["", "", "", ".####.", "######", "##..##", "##..##", "##..##", "######", ".####.", "", ""],
    "n": ["", "", "", "#####.", "######", "##..##", "##..##", "##..##", "##..##", "##..##", "", ""],
    "e": ["", "", "", ".####.", "######", "##..##", "######", "##....", "######", ".#####", "", ""],
    "s": ["", "", "", ".#####", "######", "##....", ".####.", "....##", "######", "#####.", "", ""],
}

# "app", then z o n e s in the default zone colours (green blue red yellow mauve), then the dot
THEMES = {
    "dark": {"app": "#f2ebff", "zones": ["#a6e3a1", "#89b4fa", "#f38ba8", "#f9e2af", "#cba6f7"], "dot": "#ff7cc6"},
    "light": {"app": "#2a2540", "zones": ["#40a02b", "#1e66f5", "#d20f39", "#df8e1d", "#8839ef"], "dot": "#e0499b"},
}


def pixels(theme: str) -> tuple[list[str], int]:
    """The wordmark's rects and its width in cells."""
    t = THEMES[theme]
    fills = [t["app"]] * 3 + t["zones"]
    rects, x = [], 0
    for ch, fill in zip("appzones", fills):
        rows = GLYPHS[ch]
        for y, row in enumerate(rows):
            for dx, c in enumerate(row):
                if c == "#":
                    rects.append(f'<rect x="{x + dx}" y="{y}" width="1.02" height="1.02" fill="{fill}"/>')
        x += max(len(r) for r in rows) + 1
    rects.append(f'<rect x="{x}" y="8" width="2.02" height="2.02" fill="{t["dot"]}"/>')
    return rects, x + 2


def svg(theme: str) -> str:
    rects, width = pixels(theme)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="-1 -1 {width + 2} 14" width="{(width + 2) * 8}" '
            f'height="112" role="img" aria-label="appzones" shape-rendering="crispEdges">'
            f'{"".join(rects)}</svg>\n')


TAGLINE = "Long-lived sandboxes for your desktop apps, dev tools and AI agents"


def social() -> str:
    """1280x640, dark background: the wordmark and the tagline."""
    rects, width = pixels("dark")
    cell = 14
    x0 = (1280 - width * cell) / 2
    y0 = 200
    font = "'JetBrains Mono', 'Cascadia Code', monospace"
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640">'
            f'<rect width="1280" height="640" fill="#0d1117"/>'
            f'<g transform="translate({x0} {y0}) scale({cell})" shape-rendering="crispEdges">{"".join(rects)}</g>'
            f'<text x="640" y="480" text-anchor="middle" font-family="{font}" font-size="26" '
            f'fill="#c9d1d9">{TAGLINE}</text></svg>\n')


if __name__ == "__main__":
    here = Path(__file__).resolve().parent
    for theme in THEMES:
        (here / f"appzones-{theme}.svg").write_text(svg(theme))
    (here / "social-preview.svg").write_text(social())
