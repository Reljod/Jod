#!/usr/bin/env python3
"""Stitch a before/after pair of screenshots into one labeled PNG.

Usage:
    compose_side_by_side.py before.png after.png out.png [--labels Before After]

Lays the two images side by side if their combined width stays reasonable,
otherwise stacks them vertically. Each image gets a caption bar above it.
"""
import argparse
from PIL import Image, ImageDraw, ImageFont

CAPTION_HEIGHT = 36
PADDING = 12
MAX_SIDE_BY_SIDE_WIDTH = 2400


def load_font(size: int) -> ImageFont.ImageFont:
    try:
        return ImageFont.truetype("DejaVuSans-Bold.ttf", size)
    except OSError:
        return ImageFont.load_default()


def captioned(img: Image.Image, label: str) -> Image.Image:
    font = load_font(20)
    canvas = Image.new("RGB", (img.width, img.height + CAPTION_HEIGHT), "white")
    draw = ImageDraw.Draw(canvas)
    draw.rectangle([0, 0, img.width, CAPTION_HEIGHT], fill="#222222")
    bbox = draw.textbbox((0, 0), label, font=font)
    text_w = bbox[2] - bbox[0]
    draw.text(
        ((img.width - text_w) / 2, (CAPTION_HEIGHT - (bbox[3] - bbox[1])) / 2),
        label,
        fill="white",
        font=font,
    )
    canvas.paste(img, (0, CAPTION_HEIGHT))
    return canvas


def compose(before: Image.Image, after: Image.Image, labels: tuple[str, str]) -> Image.Image:
    before_c = captioned(before, labels[0])
    after_c = captioned(after, labels[1])

    side_by_side_width = before_c.width + after_c.width + PADDING
    if side_by_side_width <= MAX_SIDE_BY_SIDE_WIDTH:
        height = max(before_c.height, after_c.height)
        out = Image.new("RGB", (side_by_side_width, height), "white")
        out.paste(before_c, (0, 0))
        out.paste(after_c, (before_c.width + PADDING, 0))
        return out

    width = max(before_c.width, after_c.width)
    height = before_c.height + after_c.height + PADDING
    out = Image.new("RGB", (width, height), "white")
    out.paste(before_c, (0, 0))
    out.paste(after_c, (0, before_c.height + PADDING))
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", help="Path to the 'before' screenshot")
    parser.add_argument("after", help="Path to the 'after' screenshot")
    parser.add_argument("out", help="Path to write the composed PNG")
    parser.add_argument(
        "--labels", nargs=2, default=["Before", "After"], metavar=("BEFORE_LABEL", "AFTER_LABEL")
    )
    args = parser.parse_args()

    before_img = Image.open(args.before).convert("RGB")
    after_img = Image.open(args.after).convert("RGB")
    result = compose(before_img, after_img, tuple(args.labels))
    result.save(args.out)
    print(f"Wrote {args.out} ({result.width}x{result.height})")


if __name__ == "__main__":
    main()
