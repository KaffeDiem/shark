"""Writes assets/themes/<name>.json from small palettes, raising text colors to the contrast floors
in docs/design.md. Linear (assets/themes/linear.json) is hand-tuned and not generated.

Usage: python3 docs/themes.py
Palettes marked "codigrate" are from github.com/codigrate/figma-color-palettes (nature/*-theme);
each codigrate theme ships one appearance, so the other mode is a sibling theme or derived here.
Popular editor themes (GitHub through Kanagawa) map each project's official palette onto KEYS;
the last five are Shark's own, one per niche the popular ones leave open.
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
    # Popular editor themes, from each project's official palette (source URL last).
    "GitHub": (  # Dark Default, Light Default; accent blue rather than the green button
        "#010409 #0d1117 #30363d #c9d1d9 #8b949e #161b22 #010409 #1f6feb #58a6ff #f85149 #3fb950 #d29922",
        "#f6f8fa #ffffff #d0d7de #24292f #57606a #ffffff #f6f8fa #0969da #0969da #cf222e #1a7f37 #9a6700",
        "https://github.com/primer/github-vscode-theme",
    ),
    "One": (  # Atom One Dark / One Light, as in Zed's one.json
        "#2f343e #282c33 #363c46 #dce0e5 #a9afbc #3b414d #21252b #74ade8 #74ade8 #d07277 #a1c181 #dec184",
        "#ebebec #fafafa #dfdfe0 #242529 #58585a #ffffff #ebebec #5c78e2 #5c78e2 #d36151 #669f59 #a48819",
        "https://github.com/zed-industries/zed/blob/main/assets/themes/one/one.json",
    ),
    "Dracula": (  # Dracula, Alucard
        "#21222c #282a36 #44475a #f8f8f2 #6272a4 #343746 #191a21 #bd93f9 #8be9fd #ff5555 #50fa7b #ffb86c",
        "#f3eed9 #fffbeb #cfcfde #1f1f1f #6c664b #ffffff #f3eed9 #644ac9 #036a96 #cb3a2a #14710a #a34d14",
        "https://draculatheme.com",
    ),
    "Catppuccin": (  # Mocha, Latte
        "#181825 #1e1e2e #313244 #cdd6f4 #7f849c #313244 #11111b #cba6f7 #89b4fa #f38ba8 #a6e3a1 #f9e2af",
        "#e6e9ef #eff1f5 #ccd0da #4c4f69 #8c8fa1 #ffffff #e6e9ef #8839ef #1e66f5 #d20f39 #40a02b #df8e1d",
        "https://github.com/catppuccin/palette",
    ),
    "Tokyo Night": (  # Night, Day
        "#16161e #1a1b26 #3b4261 #c0caf5 #565f89 #292e42 #0c0e14 #7aa2f7 #7aa2f7 #f7768e #9ece6a #e0af68",
        "#d0d5e3 #e1e2e7 #a8aecb #3760bf #848cb5 #ffffff #d0d5e3 #2e7de9 #2e7de9 #f52a65 #587539 #8c6c3e",
        "https://github.com/folke/tokyonight.nvim",
    ),
    "Gruvbox": (  # dark medium, light medium
        "#1d2021 #282828 #504945 #ebdbb2 #928374 #3c3836 #1d2021 #fe8019 #83a598 #fb4934 #b8bb26 #fabd2f",
        "#ebdbb2 #fbf1c7 #d5c4a1 #3c3836 #928374 #f9f5d7 #ebdbb2 #af3a03 #076678 #9d0006 #79740e #b57614",
        "https://github.com/morhetz/gruvbox",
    ),
    "Nord": (  # Polar Night; Nord has no light, so light is built from Snow Storm
        "#2e3440 #2e3440 #434c5e #d8dee9 #616e88 #3b4252 #2a2f3a #88c0d0 #81a1c1 #bf616a #a3be8c #ebcb8b",
        "#e5e9f0 #eceff4 #d8dee9 #2e3440 #4c566a #ffffff #e5e9f0 #5e81ac #5e81ac #bf616a #a3be8c #ebcb8b",
        "https://www.nordtheme.com",
    ),
    "Rosé Pine": (  # Main, Dawn; no blue or green, so foam/pine and leaf stand in
        "#16141f #191724 #403d52 #e0def4 #908caa #1f1d2e #16141f #c4a7e7 #9ccfd8 #eb6f92 #95b1ac #f6c177",
        "#f8f0e7 #faf4ed #dfdad9 #464261 #797593 #fffaf3 #f2e9e1 #907aa9 #286983 #b4637a #6d8f89 #ea9d34",
        "https://rosepinetheme.com",
    ),
    "Solarized": (
        "#002b36 #002b36 #073642 #839496 #586e75 #073642 #00212b #268bd2 #268bd2 #dc322f #859900 #b58900",
        "#eee8d5 #fdf6e3 #eee8d5 #657b83 #93a1a1 #ffffff #eee8d5 #268bd2 #268bd2 #dc322f #859900 #b58900",
        "https://ethanschoonover.com/solarized",
    ),
    "Ayu": (  # Mirage, Light
        "#1f2430 #242936 #33394a #cccac2 #707a8c #282e3b #181c26 #ffcc66 #73d0ff #ff6666 #87d96c #ffcd66",
        "#f8f9fa #fcfcfc #e7eaed #5c6166 #828e9f #ffffff #ebeef0 #f29718 #22a4e6 #e65050 #6cbf43 #eba400",
        "https://github.com/ayu-theme/ayu-colors",
    ),
    "Kanagawa": (  # Wave, Lotus
        "#181820 #1f1f28 #363646 #dcd7ba #727169 #2a2a37 #16161d #7e9cd8 #7e9cd8 #e82424 #98bb6c #ff9e3b",
        "#dcd5ac #f2ecbc #d5cea3 #545464 #8a8980 #f8f4d8 #dcd5ac #4d699b #4d699b #e82424 #6f894e #e98a00",
        "https://github.com/rebelot/kanagawa.nvim",
    ),
    # Shark's own, one per niche: monochrome, warm paper, true black, neon, ocean.
    "Graphite": (  # Neutral greys and no hue in the chrome (after Alabaster, Vitesse)
        "#111111 #161616 #262626 #e4e4e4 #8a8a8a #1c1c1c #0d0d0d #6e6e6e #a3b8cc #e06c6c #7bbf8e #d9b562",
        "#f0f0f0 #fafafa #e2e2e2 #1a1a1a #6b6b6b #ffffff #f0f0f0 #2b2b2b #3d5a80 #c0392b #2e7d4f #9a6b00",
        "Shark: monochrome",
    ),
    "Dune": (  # Cream paper and espresso, terracotta accent (after Flexoki, One Light, Rosé Pine Dawn)
        "#1a1612 #201b16 #342c24 #e8dfd0 #a39580 #2a241e #16120f #d9824a #7fb0d6 #e07a6c #9cbf7a #e0b45e",
        "#f0e9dc #faf6ee #e3d9c6 #2b241c #7a6b58 #fffdf8 #f0e9dc #c2652a #2f6f9f #b8433a #4f7a3a #a8740f",
        "Shark: warm paper",
    ),
    "Obsidian": (  # True black for OLED, stark white for daylight; both high contrast
        "#000000 #000000 #2a2a2a #f4f4f5 #d0d0d0 #121212 #0a0a0a #4c9aff #6cb6ff #ff6b6b #4ade80 #fbbf24",
        "#f2f2f2 #ffffff #c8c8c8 #000000 #3d3d3d #ffffff #f2f2f2 #0050d0 #0050d0 #c0001a #00702e #8a5a00",
        "Shark: true black, high contrast",
    ),
    "Neon": (  # Synthwave: hot pink and cyan on deep purple (after SynthWave '84, Shades of Purple)
        "#1a1029 #211533 #3a2752 #f0e6ff #a48cc4 #2a1c40 #150c22 #ff4fa3 #36d6ff #ff5c7a #5cf2a8 #ffd166",
        "#f3ecfb #fbf8ff #e4d8f3 #2a1640 #7a5f99 #ffffff #f3ecfb #d6247f #0b84b8 #d02a4f #138a5a #a86f00",
        "Shark: neon",
    ),
    "Lagoon": (  # Deep-sea navy and seafoam teal (after Night Owl, Noctis)
        "#031a24 #06222e #12384a #d6e9ef #7fa3b0 #0b2c3a #021219 #2bbbad #5cc8ff #ef6f6c #7fd48a #f2c46d",
        "#e6f2f3 #f6fbfb #d2e5e7 #0d2f3a #5a7d87 #ffffff #e6f2f3 #0f8a80 #1a6fa8 #c2413c #2e8a4a #9c6f00",
        "Shark: ocean",
    ),
}


def slug(name):
    return name.lower().replace(" ", "-").replace("é", "e")


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
    popover = tint(p["secondary"], 0.04) if dark else "#ffffff"
    accent = tint(p["secondary"], 0.08)
    surfaces = island + chrome + side + [popover, accent]
    mfg = lift(p["muted_fg"], surfaces, 70, dark)
    status = {k: lift(p[k], surfaces, 60, dark) for k in ("danger", "success", "warning", "link")}
    white, ink = "#ffffff", "#101010"
    on_primary = white if apca(white, p["primary"]) >= apca(ink, p["primary"]) else ink
    pr = p["primary"]
    a = lambda c, alpha: c + alpha  # noqa: E731
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
        "accent.background": accent,
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
        "chart.1": status["link"],
        "chart.2": status["success"],
        "chart.3": status["warning"],
        "chart.4": status["danger"],
        "chart.5": lift(mix(p["link"], p["danger"], 0.5), surfaces, 60, dark),
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
        url = src if src.startswith("https://") else (
            "https://github.com/KaffeDiem/shark" if src.startswith("Shark") else f"https://github.com/codigrate ({src})"
        )
        doc = {
            "name": name,
            "author": "shark",
            "url": url,
            "themes": [build(name, "dark", d), build(name, "light", l)],
        }
        (out / f"{slug(name)}.json").write_text(json.dumps(doc, indent=2) + "\n")
        print("wrote", f"{slug(name)}.json")

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
            for token, floor in [("muted.foreground", 70), ("success.foreground", 60),
                                 ("warning.foreground", 60), ("danger.foreground", 60), ("link", 60)]:
                lowest = min(apca(c[token], c[bg]) for bg in ("popover.background", "accent.background"))
                assert lowest >= floor, f"{theme['name']}: {token} menu contrast {lowest:.2f} < {floor}"
            for i in range(1, 6):
                lowest = min(apca(c[f"chart.{i}"], c[bg]) for bg in ("background", "popover.background"))
                assert lowest >= 60, f"{theme['name']}: chart.{i} contrast {lowest:.2f} < 60"
