"""The README's concept animation: what a zone can and can't reach.

    python3 assets/concept/concept.py    # writes concept-{dark,light}.svg here

A looping SVG, animated with CSS only (GitHub shows SVGs as images: no
scripts, but CSS animations play). The story, in one loop:

  1. a sketchy link comes in from the internet and opens in `untrusted`;
  2. the page tries the host's SSH key, documents and dev server, the other
     zones and the LAN: each is refused;
  3. it can still reach the internet;
  4. the caption says what that means.

Every claim is one the README and the guide already make. Where
animations don't run, the picture shows the end state: every attempt and its
result at once. It does not stop for prefers-reduced-motion: browsers on
Linux report that whenever GTK animations are off, and the README would then
never move.
"""
from pathlib import Path

W, H = 960, 640
LOOP = 14  # seconds
FONT = "ui-monospace, SFMono-Regular, 'JetBrains Mono', Menlo, Consolas, monospace"

THEMES = {
    "dark": {"text": "#e6edf3", "muted": "#8b949e", "line": "#30363d", "box": "#161b22",
             "daily": "#a6e3a1", "work": "#89b4fa", "untrusted": "#f38ba8",
             "no": "#f38ba8", "yes": "#a6e3a1", "link": "#f9e2af", "on": "#0d1117"},
    "light": {"text": "#1f2328", "muted": "#59636e", "line": "#d0d7de", "box": "#f6f8fa",
              "daily": "#40a02b", "work": "#1e66f5", "untrusted": "#d20f39",
              "no": "#d20f39", "yes": "#40a02b", "link": "#df8e1d", "on": "#ffffff"},
}

# Boxes: name -> (x, y, w, h)
UNTRUSTED = (360, 170, 240, 210)
HOST = {"ssh": (40, 170, 230, 50), "docs": (40, 250, 230, 50), "dev": (40, 330, 230, 50)}
ZONES = {"daily": (690, 170, 230, 90), "work": (690, 290, 230, 90)}
INTERNET = (380, 16, 200, 48)
LAN = (380, 524, 200, 48)

# The page's attempts, in order: (from, the target's edge, ok)
ATTEMPTS = [
    ((360, 195), (270, 195), False),   # ~/.ssh
    ((360, 275), (270, 275), False),   # ~/Documents
    ((360, 355), (270, 355), False),   # localhost:3000
    ((600, 215), (690, 215), False),   # daily
    ((600, 335), (690, 335), False),   # work
    ((480, 380), (480, 524), False),   # LAN
    ((480, 170), (480, 64), True),     # internet
]

# Timeline, in % of the loop
LINK_IN, LINK_OUT = 4, 13      # the link travels from the internet into the zone
PAGE = 13                      # "firefox: a sketchy page" appears
FIRST, STEP, DRAW = 20, 7, 3   # attempt i starts at FIRST + i*STEP, its line draws in DRAW
CAPTION = 72
HOLD = 94                      # everything stays until here, then fades for the next loop


def rect(x, y, w, h, stroke, fill, width=1.5, extra=""):
    return (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" fill="{fill}" stroke="{stroke}" '
            f'stroke-width="{width}"{extra}/>')


def text(x, y, s, fill, size=15, anchor="start", weight="normal", extra=""):
    return (f'<text x="{x}" y="{y}" fill="{fill}" font-size="{size}" text-anchor="{anchor}" '
            f'font-weight="{weight}"{extra}>{s}</text>')


def appear(name: str, start: float, fade: float = 1.5) -> str:
    """Keyframes: hidden until start, visible from start + fade until HOLD."""
    return (f"@keyframes {name}{{0%,{start}%{{opacity:0}}{start + fade}%,{HOLD}%{{opacity:1}}"
            f"{HOLD + 3}%,100%{{opacity:0}}}}")


def svg(theme: str) -> str:
    t = THEMES[theme]
    css, body = [], []

    # the laptop, the internet, the LAN
    body.append(rect(16, 96, 928, 400, t["line"], "none", 1.5, ' stroke-dasharray="6 6"'))
    body.append(text(32, 122, "alice's laptop", t["muted"], 14))
    for (x, y, w, h), label in [(INTERNET, "internet"), (LAN, "LAN: router, NAS")]:
        body.append(rect(x, y, w, h, t["line"], t["box"]))
        body.append(text(x + w / 2, y + h / 2 + 5, label, t["text"], 15, "middle"))

    # the host's things
    body.append(text(40, 156, "host", t["muted"], 14))
    for key, label in [("ssh", "~/.ssh"), ("docs", "~/Documents"), ("dev", "localhost:3000")]:
        x, y, w, h = HOST[key]
        body.append(rect(x, y, w, h, t["line"], t["box"]))
        body.append(text(x + 16, y + h / 2 + 5, label, t["text"]))

    # the other zones
    body.append(text(690, 156, "other zones", t["muted"], 14))
    for name, apps in [("daily", "telegram, firefox"), ("work", "slack, firefox")]:
        x, y, w, h = ZONES[name]
        body.append(rect(x, y, w, h, t[name], t["box"], 3))
        body.append(text(x + 16, y + 30, name, t[name], 16, weight="bold"))
        body.append(text(x + 16, y + 58, apps, t["muted"], 14))

    # the untrusted zone
    x, y, w, h = UNTRUSTED
    body.append(rect(x, y, w, h, t["untrusted"], t["box"], 3))
    body.append(text(x + 16, y + 30, "untrusted", t["untrusted"], 16, weight="bold"))
    body.append(text(x + 16, y + 58, "its own home", t["muted"], 14))
    css.append(appear("page", PAGE))
    body.append(f'<g class="a" style="animation-name:page">'
                + text(x + 16, y + 110, "firefox:", t["text"], 15)
                + text(x + 16, y + 132, "a sketchy page", t["link"], 15, weight="bold")
                + "</g>")

    # the link, from the internet into the zone
    lx, ly = INTERNET[0] + INTERNET[2] / 2, INTERNET[1] + INTERNET[3] / 2
    css.append(f"@keyframes link{{0%,{LINK_IN}%{{opacity:0;transform:translate(0,0)}}"
               f"{LINK_IN + 1}%{{opacity:1;transform:translate(0,0)}}"
               f"{LINK_OUT - 1}%{{opacity:1;transform:translate(0,{UNTRUSTED[1] + 95 - ly}px)}}"
               f"{LINK_OUT}%,100%{{opacity:0;transform:translate(0,{UNTRUSTED[1] + 95 - ly}px)}}}}")
    body.append(f'<g class="a link" style="animation-name:link">'
                + rect(lx - 62, ly - 14, 124, 28, t["link"], t["link"], 1)
                + text(lx, ly + 5, "a link", t["on"], 14, "middle", "bold")
                + "</g>")

    # the attempts
    for i, ((x1, y1), (x2, y2), ok) in enumerate(ATTEMPTS):
        start = FIRST + i * STEP
        colour = t["yes"] if ok else t["no"]
        if not ok:  # a refused attempt stops at its mark, short of the target
            x2, y2 = x1 + (x2 - x1) * 0.6, y1 + (y2 - y1) * 0.6
        length = abs(x2 - x1) + abs(y2 - y1)
        css.append(f"@keyframes l{i}{{0%,{start}%{{stroke-dashoffset:{length};opacity:0}}"
                   f"{start + 0.2}%{{stroke-dashoffset:{length};opacity:1}}"
                   f"{start + DRAW}%,{HOLD}%{{stroke-dashoffset:0;opacity:1}}"
                   f"{HOLD + 3}%,100%{{stroke-dashoffset:0;opacity:0}}}}")
        body.append(f'<line class="a" style="animation-name:l{i}" x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" '
                    f'stroke="{colour}" stroke-width="2.5" stroke-dasharray="{length}"'
                    + (f' marker-end="url(#yes-{theme})"/>' if ok else "/>"))
        # the verdict: where a refused line stops, on an allowed line's middle
        mx, my = ((x1 + x2) / 2, (y1 + y2) / 2) if ok else (x2, y2)
        css.append(appear(f"m{i}", start + DRAW, 0.8))
        if ok:
            mark = (f'<circle cx="{mx}" cy="{my}" r="13" fill="{t["box"]}" stroke="{colour}" stroke-width="2.5"/>'
                    f'<path d="M{mx - 6} {my} l4 5 l8 -10" fill="none" stroke="{colour}" stroke-width="3" '
                    f'stroke-linecap="round" stroke-linejoin="round"/>')
        else:
            mark = (f'<circle cx="{mx}" cy="{my}" r="13" fill="{t["box"]}" stroke="{colour}" stroke-width="2.5"/>'
                    f'<path d="M{mx - 5} {my - 5} l10 10 M{mx + 5} {my - 5} l-10 10" stroke="{colour}" '
                    f'stroke-width="3" stroke-linecap="round"/>')
        body.append(f'<g class="a" style="animation-name:m{i}">{mark}</g>')

    # the caption
    css.append(appear("cap", CAPTION, 2))
    body.append(f'<g class="a" style="animation-name:cap">'
                + text(W / 2, 610, "A zone can't see your home, your LAN, the host or other zones.",
                       t["text"], 17, "middle", "bold")
                + text(W / 2, 632, "It still gets the internet.", t["muted"], 15, "middle")
                + "</g>")

    markers = "".join(
        f'<marker id="{k}-{theme}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" '
        f'orient="auto-start-reverse"><path d="M0 0 L10 5 L0 10 z" fill="{t[k]}"/></marker>'
        for k in ("yes",))
    style = (f".a{{animation-duration:{LOOP}s;animation-iteration-count:infinite;"
             f"animation-timing-function:ease-in-out}}"
             + "".join(css)
             + ".link{opacity:0}")
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" '
            f'role="img" aria-label="A link opens in the untrusted zone. Its attempts to read ~/.ssh and '
            f'~/Documents, reach the host\'s dev server, the other zones and the LAN are refused; the internet '
            f'is reachable." font-family="{FONT}">'
            f"<defs>{markers}<style>{style}</style></defs>"
            + "".join(body) + "</svg>\n")


if __name__ == "__main__":
    here = Path(__file__).resolve().parent
    for theme in THEMES:
        (here / f"concept-{theme}.svg").write_text(svg(theme))
