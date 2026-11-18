"""Draws macOS's close, minimize and zoom buttons onto Feeder snapshots, which headless runs
render without them (the system draws them over a real window). Positions match the app's
titlebar: the close button's frame starts 18 pt in and 17 pt down, buttons are 14 pt frames with
12 pt circles, 20 pt apart.

    uvx --with pillow python scripts/site/window-buttons.py <scale> <in.png> <out.png>
"""

import sys

from PIL import Image, ImageDraw

BUTTONS = [("#ff5f57", "#e0443e"), ("#febc2e", "#dea123"), ("#28c840", "#1aab29")]


def main(scale: float, src: str, dst: str) -> None:
    image = Image.open(src).convert("RGBA")
    layer = Image.new("RGBA", image.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    for i, (fill, edge) in enumerate(BUTTONS):
        cx, cy, r = (18 + 7 + 20 * i) * scale, (17 + 7) * scale, 6 * scale
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=fill, outline=edge, width=max(1, round(0.5 * scale)))
    Image.alpha_composite(image, layer).convert("RGB").save(dst, optimize=True)


if __name__ == "__main__":
    main(float(sys.argv[1]), sys.argv[2], sys.argv[3])
