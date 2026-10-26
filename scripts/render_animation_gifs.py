#!/usr/bin/env python3
"""Render compact README GIFs from VTCode's actual terminal buffers.

Requires Pillow. First capture the five scenes with:
  cargo test --locked --release --bin vtcode export_readme_animation_frames -- --ignored
Then run this script. Source frames stay in target/; only the GIFs are published.
"""

from __future__ import annotations

import argparse
from functools import lru_cache
import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
SLUGS = (
    "cursor-smear", "fire-hazard", "whitespace-reveal", "scope-connector", "sliding-tabs",
)
CELL_WIDTH, CELL_HEIGHT, PADDING = 10, 20, 8
QUADRANTS = {
    "▘": (0,), "▝": (1,), "▖": (2,), "▗": (3,), "▚": (0, 3),
    "▞": (1, 2), "▙": (0, 2, 3), "▛": (0, 1, 2), "▜": (0, 1, 3), "▟": (1, 2, 3),
}


def load_font(path: str | None, bold: bool = False, italic: bool = False):
    face = {(False, False): "", (True, False): "-Bold",
            (False, True): "-Oblique", (True, True): "-BoldOblique"}[bold, italic]
    consola = {(False, False): "consola", (True, False): "consolab",
               (False, True): "consolai", (True, True): "consolaz"}[bold, italic]
    candidates = [path] if path else [
        f"C:/Windows/Fonts/DejaVuSansMono{face}.ttf",
        f"DejaVuSansMono{face}.ttf",
        f"C:/Windows/Fonts/{consola}.ttf",
    ]
    for candidate in candidates:
        try:
            return ImageFont.truetype(candidate, 16, layout_engine=ImageFont.Layout.BASIC)
        except OSError:
            continue
    raise SystemExit("No monospace font found; supply --font /path/to/font.ttf")


def renderer(font_path: str | None):
    fonts = {(bold, italic): load_font(font_path, bold, italic)
             for bold in (False, True) for italic in (False, True)}
    try:
        symbols = ImageFont.truetype("C:/Windows/Fonts/seguisym.ttf", 16)
    except OSError:
        symbols = fonts[False, False]

    @lru_cache(maxsize=32768)
    def tile(symbol, fg, bg, is_bold, italic, underline):
        image = Image.new("RGB", (CELL_WIDTH, CELL_HEIGHT), bg)
        draw = ImageDraw.Draw(image)
        w, h = CELL_WIDTH, CELL_HEIGHT
        if symbol == "█":
            draw.rectangle((0, 0, w - 1, h - 1), fill=fg)
        elif symbol and symbol in "▁▂▃▄▅▆▇":
            height = round(("▁▂▃▄▅▆▇".index(symbol) + 1) * h / 8)
            draw.rectangle((0, h - height, w - 1, h - 1), fill=fg)
        elif symbol and symbol in "▏▎▍▌▋▊▉":
            width = round(("▏▎▍▌▋▊▉".index(symbol) + 1) * w / 8)
            draw.rectangle((0, 0, width - 1, h - 1), fill=fg)
        elif symbol in ("▀", "▔", "▐", "▕"):
            bounds = {
                "▀": (0, 0, w - 1, h // 2 - 1), "▔": (0, 0, w - 1, h // 8 - 1),
                "▐": (w // 2, 0, w - 1, h - 1), "▕": (w * 7 // 8, 0, w - 1, h - 1),
            }
            draw.rectangle(bounds[symbol], fill=fg)
        elif symbol in QUADRANTS:
            for quadrant in QUADRANTS[symbol]:
                x, y = quadrant % 2 * w // 2, quadrant // 2 * h // 2
                draw.rectangle((x, y, x + w // 2 - 1, y + h // 2 - 1), fill=fg)
        elif len(symbol) == 1 and 0x2800 <= ord(symbol) <= 0x28FF:
            for bit, (column, row) in enumerate(((0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3))):
                if (ord(symbol) - 0x2800) & (1 << bit):
                    x, y = 2 + column * 5, 2 + row * 5
                    draw.rectangle((x, y, x + 1, y + 1), fill=fg)
        elif symbol != " ":
            selected_font = fonts[is_bold, italic]
            if len(symbol) == 1 and ord(symbol) > 0xFFFF:
                selected_font = symbols
            x = (w - selected_font.getlength(symbol)) / 2
            ascent, descent = selected_font.getmetrics()
            draw.text((x, (h - ascent - descent) // 2 + ascent), symbol,
                      font=selected_font, fill=fg, anchor="ls")
        if underline:
            draw.line((0, h - 2, w - 1, h - 2), fill=underline, width=1)
        return image

    def render(scene, cells):
        width, height = scene["width"], scene["height"]
        size = (width * CELL_WIDTH + PADDING * 2, height * CELL_HEIGHT + PADDING * 2)
        image = Image.new("RGB", size, "#161b22")
        draw = ImageDraw.Draw(image)
        draw.rounded_rectangle((0, 0, size[0] - 1, size[1] - 1), radius=8,
                               fill=cells[0][2], outline="#3b414b", width=1)
        for index, cell in enumerate(cells):
            x = PADDING + index % width * CELL_WIDTH
            y = PADDING + index // width * CELL_HEIGHT
            image.paste(tile(*cell), (x, y))
        return image

    return render


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--frames-dir", type=Path, default=ROOT / "target/readme-animations")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "screenshots/animations")
    parser.add_argument("--font", help="Optional monospace TrueType font")
    parser.add_argument("--contact-sheet", type=Path, help="Optional stills for visual review")
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    render = renderer(args.font)
    previews = []
    for slug in SLUGS:
        path = args.frames_dir / f"{slug}.json"
        if not path.exists():
            raise SystemExit(f"Missing {path}; run the capture command in --help first.")
        scene = json.loads(path.read_text(encoding="utf-8"))
        frames = [render(scene, cells) for cells in scene["frames"]]
        # One palette per clip prevents shimmer in static text. Keep dithering
        # off so quiet backgrounds stay clean and GIF delta compression is small.
        samples = frames[::8]
        montage = Image.new("RGB", (frames[0].width, frames[0].height * len(samples)))
        for index, sample in enumerate(samples):
            montage.paste(sample, (0, index * sample.height))
        palette = montage.quantize(colors=256, method=Image.Quantize.MEDIANCUT)
        indexed = [frame.quantize(palette=palette, dither=Image.Dither.NONE) for frame in frames]
        # GIF timing has 10 ms precision: alternate 30/40/30 ms for exact 30 FPS.
        durations = [40 if index % 3 == 1 else 30 for index in range(len(frames))]
        output = args.output_dir / f"{slug}.gif"
        indexed[0].save(output, save_all=True, append_images=indexed[1:],
                        duration=durations, loop=0, optimize=True, disposal=1)
        with Image.open(output) as gif:
            assert gif.n_frames > 10 and gif.info["loop"] == 0
            assert gif.size == frames[0].size
            elapsed = 0
            for index in range(gif.n_frames):
                gif.seek(index)
                gif.load()
                elapsed += gif.info["duration"]
            assert elapsed == sum(durations), "GIF timing must match the captured loop"
        print(f"{output.relative_to(ROOT) if output.is_relative_to(ROOT) else output}: "
              f"{frames[0].width}x{frames[0].height}, {output.stat().st_size / 1024:.0f} KiB")
        previews.append((scene["name"], [frames[index] for index in (20, 52, 96, 146)]))
    if args.contact_sheet:
        width, height = previews[0][1][0].size
        sheet = Image.new("RGB", (width * 4, (height + 28) * len(previews)), "#161b22")
        draw = ImageDraw.Draw(sheet)
        font = load_font(args.font)
        for row, (name, stills) in enumerate(previews):
            y = row * (height + 28)
            draw.text((8, y + 4), name, font=font, fill="#e6edf3")
            for column, frame in enumerate(stills):
                sheet.paste(frame, (column * width, y + 28))
        args.contact_sheet.parent.mkdir(parents=True, exist_ok=True)
        sheet.save(args.contact_sheet)


if __name__ == "__main__":
    main()
