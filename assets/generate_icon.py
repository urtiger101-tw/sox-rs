"""Regenerate the Windows application icon (requires Pillow)."""

from pathlib import Path

from PIL import Image, ImageDraw


def render(size: int) -> Image.Image:
    scale = 4
    edge = size * scale
    canvas = Image.new("RGBA", (edge, edge), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)
    draw.rounded_rectangle(
        (2 * scale, 2 * scale, edge - 2 * scale, edge - 2 * scale),
        radius=edge // 4,
        fill="#13253c",
    )
    draw.ellipse(
        (10 * scale, 10 * scale, edge - 10 * scale, edge - 10 * scale),
        outline="#29d2c2",
        width=max(2, edge // 45),
    )
    heights = [0.18, 0.36, 0.52, 0.36, 0.18]
    for index, height in enumerate(heights):
        x = int(edge * (0.31 + 0.095 * index))
        half = int(edge * height / 2)
        draw.rounded_rectangle(
            (x - edge // 35, edge // 2 - half, x + edge // 35, edge // 2 + half),
            radius=edge // 35,
            fill="#f1fbff" if index == 2 else "#29d2c2",
        )
    return canvas.resize((size, size), Image.Resampling.LANCZOS)


if __name__ == "__main__":
    destination = Path(__file__).with_name("soundx.ico")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    render(256).save(destination, format="ICO", sizes=[(size, size) for size in sizes])
    print(destination)
