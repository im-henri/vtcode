"""Render the actual Ratatui buffers captured by the VTComplete UI preview test.

Requires Pillow. Pass the capture directory; PNGs are written alongside JSON.
These previews check terminal layout and theme colors, not native window chrome.
"""

import argparse
import json
import re
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


ANSI = [
    (0, 0, 0), (170, 0, 0), (0, 170, 0), (170, 85, 0),
    (0, 0, 170), (170, 0, 170), (0, 170, 170), (170, 170, 170),
    (85, 85, 85), (255, 85, 85), (85, 255, 85), (255, 255, 85),
    (85, 85, 255), (255, 85, 255), (85, 255, 255), (255, 255, 255),
]
NAMES = dict(zip(
    ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "Gray",
     "DarkGray", "LightRed", "LightGreen", "LightYellow", "LightBlue",
     "LightMagenta", "LightCyan", "White"], ANSI
))


def color(value, fallback):
    if value.startswith("Rgb("):
        return tuple(map(int, re.findall(r"\d+", value)))
    if value.startswith("Indexed("):
        index = int(re.search(r"\d+", value).group())
        if index < 16:
            return ANSI[index]
        if index >= 232:
            return (8 + (index - 232) * 10,) * 3
        index -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return levels[index // 36], levels[(index // 6) % 6], levels[index % 6]
    return NAMES.get(value, fallback)


def font_paths(requested):
    if requested:
        normal = Path(requested)
        return normal, normal
    windows = Path("C:/Windows/Fonts")
    if (windows / "consola.ttf").exists():
        return windows / "consola.ttf", windows / "consolab.ttf"
    linux = Path("/usr/share/fonts/truetype/dejavu")
    return linux / "DejaVuSansMono.ttf", linux / "DejaVuSansMono-Bold.ttf"


def render(path, normal, bold, cell_width, cell_height):
    data = json.loads(path.read_text(encoding="utf-8"))
    cols, rows = data["width"], data["height"]
    result = Image.new("RGB", (cols * cell_width, rows * cell_height))
    draw = ImageDraw.Draw(result)
    for i, cell in enumerate(data["cells"]):
        x, y = i % cols * cell_width, i // cols * cell_height
        bg = color(cell["bg"], (24, 24, 24))
        draw.rectangle((x, y, x + cell_width - 1, y + cell_height - 1), fill=bg)
    for i, cell in enumerate(data["cells"]):
        if cell["text"].strip():
            x, y = i % cols * cell_width, i // cols * cell_height
            draw.text((x, y + 1), cell["text"], font=bold if cell["bold"] else normal,
                      fill=color(cell["fg"], (220, 220, 220)))
            if cell.get("underline"):
                draw.line((x, y + cell_height - 3, x + cell_width - 1, y + cell_height - 3),
                          fill=color(cell["fg"], (220, 220, 220)))
    result.save(path.with_suffix(".png"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--font", help="Monospace TrueType font path")
    parser.add_argument("--size", type=int, default=18)
    args = parser.parse_args()
    regular, heavy = font_paths(args.font)
    normal, bold = (ImageFont.truetype(str(path), args.size) for path in (regular, heavy))
    cell_width = round(normal.getlength("M"))
    cell_height = round(args.size * 1.4)
    paths = sorted(args.directory.glob("*.json"))
    for path in paths:
        render(path, normal, bold, cell_width, cell_height)
    print(f"Rendered {len(paths)} terminal buffers in {args.directory}")


if __name__ == "__main__":
    main()
