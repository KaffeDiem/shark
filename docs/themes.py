"""Writes assets/themes/<name>.json from small palettes, raising text colors to the contrast floors
in docs/design.md. Linear (assets/themes/linear.json) is hand-tuned and not generated.

Usage: python3 docs/themes.py
Palettes marked "codigrate" are from github.com/codigrate/figma-color-palettes (nature/*-theme);
each codigrate theme ships one appearance, so the other mode is a sibling theme or derived here.
"""
import json
import math
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from contrast import apca, wcag  # noqa: E402

KEYS = "sidebar background border foreground muted_fg secondary muted primary link danger success warning".split()
# name: (dark palette, light palette, sources)
THEMES = {
    "Roraima": (  # dark: codigrate Roraima; light: codigrate Autumn
        "#322628 #1e1a1b #372c2f #cec1c1 #ad8a8a #372c2f #171415 #cc654e #5292c7 #ce4a44 #4cc27b #cda644",
        "#f8f4f1 #fcfbfa #efe6e0 #2e1b0f #82614c #ffffff #f8f4f1 #a7714c #397fb7 #c8504a #30a25e #d49a2a",
        "codigrate Roraima (dark), codigrate Autumn (light)",
    ),
    "Everest": (  # light: codigrate Everest; dark: codigrate Aurora Borealis
        "#123243 #142b37 #1c3847 #b0b9be #85a2b3 #1a3746 #112733 #7ac6f5 #5aa6da #d0636c #5bc093 #cba45e",
        "#e4ecef #fdfeff #cadae0 #0e3448 #567b8a #ffffff #e4ecef #246a89 #397fb7 #c8534e #30a25e #c8963e",
        "codigrate Aurora Borealis (dark), codigrate Everest (light)",
    ),
    "Sequoia": (  # dark: codigrate Sequoia; light derived
        "#32382c #20231c #394132 #babeba #849184 #394132 #0e0e0e #73a621 #4a93c0 #c2564b #5cb36a #c39a45",
        "#f1f3eb #fbfcf8 #dfe3d5 #232a1c #5d6b52 #ffffff #f1f3eb #5a8418 #3b7aa3 #b8463c #2f8a3e #8a6a10",
        "codigrate Sequoia (dark), light derived",
    ),
    "Sakura": (  # light: codigrate Sakura; dark derived
        "#1f151a #261a20 #3a2830 #ead9e0 #c29aae #30212a #1d1418 #e07a96 #8ab4e8 #f08a84 #6ccb8a #e3c070",
        "#fcf4f7 #fefcfc #f8dbe6 #3d0013 #94667d #ffffff #fcf4f7 #b54b66 #397fb7 #be5c55 #30a25e #bc9c5c",
        "codigrate Sakura (light), dark derived",
    ),
}


def rgb(h):
    h = h.lstrip("#")
    return [int(h[i : i + 2], 16) for i in (0, 2, 4)]


def hexa(c):
    return "#" + "".join(f"{round(max(0, min(255, x))):02x}" for x in c)


def mix(a, b, t):
    """`a` moved `t` of the way to `b`."""
    return hexa([x + (y - x) * t for x, y in zip(rgb(a), rgb(b))])


def lift(fg, bgs, floor, dark, contrast=apca):
    """Moves `fg` toward white (dark) or black (light) until it reaches `floor` on every bg."""
    target = "#f4f4f5" if dark else "#000000"  # no pure white on near-black
    for i in range(101):
        c = mix(fg, target, i / 100)
        if min(contrast(c, b) for b in bgs) >= floor:
            return c
    return target


def navigation_surfaces(sidebar, fg, accent, dark):
    surfaces = [sidebar, accent]
    # Round toward lower contrast so fractional GPU blends also meet the floor.
    rounding = math.ceil if dark else math.floor
    for t in ((0.07, 0.11) if dark else (0.03, 0.05)):
        surfaces.append(hexa([
            rounding(x + (y - x) * t) for x, y in zip(rgb(sidebar), rgb(fg))
        ]))
    return surfaces


def build(name, mode, pal):
    p = dict(zip(KEYS, pal.split()))
    dark = mode == "dark"
    fg = p["foreground"]
    tint = lambda a, t: mix(a, fg, t)  # noqa: E731
    list_hover = tint(p["background"], 0.06)
    sidebar_accent = tint(p["sidebar"], 0.07)
    list_active = mix(p["background"], p["primary"], 0.2 if dark else 0.06)
    island = [p["background"], p["secondary"], p["muted"], list_hover, list_active]
    chrome = [p["sidebar"], sidebar_accent]
    fg = lift(lift(fg, [p["background"], p["secondary"]], 90, dark), island + chrome, 70, dark)
    # `side_hover()` and `side_on()` in src/app.rs: the final `foreground` over `sidebar`.
    side = [mix(p["sidebar"], fg, t) for t in ((0.07, 0.11) if dark else (0.03, 0.05))]
    assert min(apca(fg, b) for b in side) >= 70, f"{name} {mode}: foreground on side rows"
    mfg = lift(p["muted_fg"], island + chrome + side, 70, dark)
    status = {k: lift(p[k], island + chrome + side, 60, dark) for k in ("danger", "success", "warning", "link")}
    white, ink = "#ffffff", "#101010"
    on_primary = white if apca(white, p["primary"]) >= apca(ink, p["primary"]) else ink
    pr = p["primary"]
    a = lambda c, alpha: c + alpha  # noqa: E731
    popover = tint(p["secondary"], 0.04) if dark else "#ffffff"
    colors = {
        "background": p["background"],
        "foreground": fg,
        "border": p["border"],
        "input.border": tint(p["border"], 0.08),
        "ring": pr,
        "caret": fg,
        "selection.background": a(pr, "55" if dark else "40"),
        "overlay": "#00000066" if dark else "#0000001a",
        "window.border": p["border"],
        "muted.background": p["muted"],
        "muted.foreground": mfg,
        "accent.background": tint(p["secondary"], 0.08),
        "accent.foreground": fg,
        "secondary.background": p["secondary"],
        "secondary.hover.background": tint(p["secondary"], 0.05),
        "secondary.active.background": tint(p["secondary"], 0.08),
        "secondary.foreground": fg,
        "primary.background": pr,
        "primary.hover.background": mix(pr, "#000000", 0.08),
        "primary.active.background": mix(pr, "#000000", 0.16),
        "primary.foreground": on_primary,
        "popover.background": popover,
        "popover.foreground": fg,
        "link": status["link"],
        "link.hover": tint(status["link"], 0.2),
        "link.active": status["link"],
        "list.background": p["background"],
        "list.hover.background": list_hover,
        "list.active.background": a(pr, "33" if dark else "0f"),
        "list.active.border": pr,
        "list.even.background": p["background"],
        "list.head.background": p["background"],
        "sidebar.background": p["sidebar"],
        "sidebar.foreground": lift(
            mix(mfg, p["sidebar"], 0.3),
            navigation_surfaces(p["sidebar"], fg, sidebar_accent, dark), 4.5, dark, wcag,
        ),
        "sidebar.border": p["sidebar"],
        "sidebar.accent.background": sidebar_accent,
        "sidebar.accent.foreground": fg,
        "sidebar.primary.background": pr,
        "sidebar.primary.foreground": on_primary,
        "tab.background": "#00000000",
        "tab.foreground": mfg,
        "tab.active.background": p["secondary"],
        "tab.active.foreground": fg,
        "tab_bar.background": p["background"],
        "tab_bar.segmented.background": p["background"],
        "title_bar.background": p["sidebar"],
        "title_bar.border": p["sidebar"],
        "status_bar.background": p["sidebar"],
        "status_bar.border": p["border"],
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": a(tint(p["background"], 0.2), "e6"),
        "scrollbar.thumb.hover.background": tint(p["background"], 0.3),
        "skeleton.background": p["secondary"],
        "switch.background": tint(p["background"], 0.2),
        "success.background": p["success"],
        "success.foreground": status["success"],
        "warning.background": p["warning"],
        "warning.foreground": status["warning"],
        "danger.background": p["danger"],
        "danger.foreground": status["danger"],
        "info.background": pr,
        "info.foreground": status["link"],
    }
    # Same blend as `faint()` in src/app.rs.
    faint = mix(mfg, p["background"], 0.4)
    low = min(apca(faint, b) for b in island + chrome)
    assert low >= 30, f"{name} {mode}: faint Lc {low:.1f} < 30"
    return {
        "name": f"{name} {mode.title()}",
        "mode": mode,
        "font.family": "Inter",
        "font.size": 15,
        "mono_font.family": "JetBrains Mono NL",
        "mono_font.size": 12.5,
        "radius": 8,
        "radius.lg": 12,
        "shadow": False,
        "colors": colors,
    }


if __name__ == "__main__":
    out = pathlib.Path(__file__).parent.parent / "assets/themes"
    for name, (d, l, src) in THEMES.items():
        doc = {
            "name": name,
            "author": "shark",
            "url": f"https://github.com/codigrate ({src})",
            "themes": [build(name, "dark", d), build(name, "light", l)],
        }
        (out / f"{name.lower()}.json").write_text(json.dumps(doc, indent=2) + "\n")
        print("wrote", f"{name.lower()}.json")

    for path in sorted(out.glob("*.json")):
        for theme in json.loads(path.read_text())["themes"]:
            c = theme["colors"]
            dark = theme["mode"] == "dark"
            surfaces = navigation_surfaces(
                c["sidebar.background"], c["foreground"], c["sidebar.accent.background"], dark,
            )
            lowest = min(wcag(c["sidebar.foreground"], bg) for bg in surfaces)
            assert lowest >= 4.5, f"{theme['name']}: navigation contrast {lowest:.2f}:1 < 4.5:1"
            print(f"{theme['name']}: navigation minimum {lowest:.2f}:1")
