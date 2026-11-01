"""Usage: python3 android/make-icon.py crates/app/assets/bliss.ico android/app/src/main/res

The adaptive launcher icon's layers, from the desktop's bliss.ico.

The 256 px frame holds the mark (the b, its ring and the star) over the
wordmark on a dark plate. The mark is lifted off the plate by colour to
alpha against the plate's colour, so laid back on a ground of that colour it
looks as it did, then centred in Android's 108 dp canvas inside the 66 dp
circle every launcher mask keeps.
"""
import sys
from pathlib import Path
from PIL import Image

ico_path, res = Path(sys.argv[1]), Path(sys.argv[2])
ico = Image.open(ico_path)
ico.size = (256, 256)
src = ico.convert("RGBA")

BG = (17, 9, 34)          # the plate, sampled across it
FLOOR = 0.10              # the plate's own gradient, not the mark
box = (24, 36, 232, 180)  # the mark, above the wordmark

crop = src.crop(box)
out = Image.new("RGBA", crop.size)
cp, op = crop.load(), out.load()
for y in range(crop.size[1]):
    for x in range(crop.size[0]):
        c = cp[x, y][:3]
        # Only what is brighter than the plate: darker is the plate's own
        # shading, which showed as a band under the mark.
        a = max(max(c[i] - BG[i], 0) / (255 - BG[i]) for i in range(3))
        if a <= FLOOR:
            op[x, y] = (0, 0, 0, 0)
            continue
        colour = tuple(
            max(0, min(255, round(BG[i] + (c[i] - BG[i]) / a))) for i in range(3)
        )
        a2 = (a - FLOOR) / (1 - FLOOR)
        op[x, y] = (*colour, round(255 * a2))

bbox = out.getbbox()
mark = out.crop(bbox)
w, h = mark.size
print("mark", w, h, "from", bbox)

DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}
for name, scale in DENSITIES.items():
    side = round(108 * scale)
    circle = 66 * scale  # the safe circle every launcher mask keeps
    # The mark's diagonal fits the circle.
    k = circle / (w * w + h * h) ** 0.5
    size = (round(w * k), round(h * k))
    scaled = mark.resize(size, Image.LANCZOS)
    canvas = Image.new("RGBA", (side, side))
    canvas.paste(scaled, ((side - size[0]) // 2, (side - size[1]) // 2), scaled)
    folder = res / f"mipmap-{name}"
    folder.mkdir(parents=True, exist_ok=True)
    canvas.save(folder / "ic_launcher_foreground.png", optimize=True)
    alpha = canvas.getchannel("A")
    mono = Image.new("RGBA", (side, side), (255, 255, 255, 0))
    mono.putalpha(alpha)
    mono.save(folder / "ic_launcher_monochrome.png", optimize=True)
    print(name, side, size)
