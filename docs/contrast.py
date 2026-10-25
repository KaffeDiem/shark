"""APCA (W3 0.0.98G) Lc and WCAG 2 ratio for text/background pairs.

Usage: python3 docs/contrast.py '#e8e9eb' '#121213' ['#fg' '#bg' ...]
"""
import sys


def _y(h):
    h = h.lstrip("#")
    return sum(w * (int(h[i : i + 2], 16) / 255) ** 2.4 for w, i in zip((0.2126729, 0.7151522, 0.0721750), (0, 2, 4)))


def apca(text, bg):
    clamp = lambda y: y if y > 0.022 else y + (0.022 - y) ** 1.414
    yt, yb = clamp(_y(text)), clamp(_y(bg))
    s = (yb**0.56 - yt**0.57) * 1.14 if yb > yt else (yb**0.65 - yt**0.62) * 1.14
    return 0.0 if abs(s) < 0.1 else (abs(s) - 0.027) * 100


def wcag(a, b):
    def lum(h):
        h = h.lstrip("#")
        c = [int(h[i : i + 2], 16) / 255 for i in (0, 2, 4)]
        c = [x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in c]
        return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]

    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


if __name__ == "__main__":
    assert round(apca("#000000", "#ffffff")) == 106 and round(wcag("#000000", "#ffffff")) == 21
    args = sys.argv[1:]
    for fg, bg in zip(args[::2], args[1::2]):
        print(f"{fg} on {bg}: Lc {apca(fg, bg):.1f}, {wcag(fg, bg):.2f}:1")
