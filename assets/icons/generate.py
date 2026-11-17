"""Regenerate the Oxeye icons with Python 3 and Pillow."""

from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parent
SCALE = 4
image = Image.new("RGBA", (1024 * SCALE, 1024 * SCALE))
draw = ImageDraw.Draw(image)


def box(bounds):
    return tuple(value * SCALE for value in bounds)


def curve(points):
    return [
        tuple(
            SCALE
            * (
                (1 - t) ** 3 * points[0][axis]
                + 3 * (1 - t) ** 2 * t * points[1][axis]
                + 3 * (1 - t) * t**2 * points[2][axis]
                + t**3 * points[3][axis]
            )
            for axis in (0, 1)
        )
        for t in (step / 256 for step in range(257))
    ]


draw.rounded_rectangle(box((32, 32, 992, 992)), radius=224 * SCALE, fill="#244f87")
eye = curve(((156, 512), (330, 256), (694, 256), (868, 512)))
eye += curve(((868, 512), (694, 768), (330, 768), (156, 512)))
draw.polygon(eye, fill="#e4edfc")
draw.ellipse(box((352, 352, 672, 672)), fill="#86b9ff")
draw.ellipse(box((438, 438, 586, 586)), fill="#142b46")

canonical = image.resize((1024, 1024), Image.Resampling.LANCZOS)
for size in (1024, 512, 256):
    canonical.resize((size, size), Image.Resampling.LANCZOS).save(
        ROOT / f"oxeye-{size}.png", optimize=True
    )
canonical.save(
    ROOT / "oxeye.ico",
    sizes=[(size, size) for size in (16, 32, 48, 64, 128, 256)],
)

# Check the actual encoded files, including every embedded ICO size.
for size in (1024, 512, 256):
    with Image.open(ROOT / f"oxeye-{size}.png") as icon:
        assert icon.size == (size, size) and icon.mode == "RGBA"
        assert icon.getpixel((0, 0))[3] == 0
with Image.open(ROOT / "oxeye.ico") as icon:
    assert {(16, 16), (32, 32), (48, 48), (256, 256)} <= icon.ico.sizes()
    for size in icon.ico.sizes():
        frame = icon.ico.getimage(size)
        assert frame.size == size and frame.mode == "RGBA"
assert sum(path.stat().st_size for path in ROOT.iterdir() if path.is_file()) < 1_000_000
