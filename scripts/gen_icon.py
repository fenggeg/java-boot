"""Generate JavaBoot Launcher icons — crisp at every size.

Strategy:
- A bold, filled emblem (no thin strokes) so it reads at 16px.
- Small sizes (<=64) are drawn NATIVELY at target pixel size for sharpness.
- Large sizes are rendered from a 1024 source.
"""
from PIL import Image, ImageDraw, ImageFilter
import os

# palette
BG = (10, 11, 13, 255)
BG_HI = (28, 32, 37, 255)
LIME = (163, 230, 53, 255)
LIME_DIM = (132, 204, 22, 255)


def draw_emblem(d, sz, scale):
    """Draw the emblem on draw object `d` at canvas size `sz`.
    scale = sz / 32  (design viewBox is 32x32)
    Use bold filled shapes only — no thin strokes."""
    def P(x, y):
        return (x * scale, y * scale)

    # ── Bold bracket frame: two thick L-shapes (top-left + bottom-right) ──
    t = 3.2  # frame thickness in viewBox units (bold)
    # top-left L
    d.polygon([
        P(4, 4), P(4 + t, 4), P(4 + t, 4 + 28 - t), P(4 + 28 - t, 4 + 28 - t),
        P(4 + 28 - t, 4 + 28), P(4, 4 + 28)
    ], fill=LIME_DIM)
    # cut inner to leave just the L-frame ring
    d.polygon([
        P(4 + t, 4 + t), P(4 + 28 - t, 4 + t), P(4 + 28 - t, 4 + 28 - t),
        P(4 + t, 4 + 28 - t)
    ], fill=BG)
    # re-open the right side so it's a bracket "[" not a full frame
    d.rectangle([P(4 + 28 - t, 4), P(4 + 28, 4 + 28)], fill=BG)

    # ── Bold play triangle (filled, centered) ──
    tri = [P(12, 10), P(22, 16), P(12, 22)]
    d.polygon(tri, fill=LIME)

    # ── Bold baseline rule ──
    d.rectangle([P(12, 24.5), P(22, 24.5 + 2.4)], fill=LIME)


def make_icon(sz):
    """Render the icon at exact pixel size `sz` — native, no downscale."""
    img = Image.new("RGBA", (sz, sz), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # rounded-square dark background
    radius = int(sz * 0.22)
    d.rounded_rectangle([0, 0, sz - 1, sz - 1], radius=radius, fill=BG)

    # subtle center highlight (skip for tiny sizes, keeps them crisp)
    if sz >= 64:
        hl = Image.new("RGBA", (sz, sz), (0, 0, 0, 0))
        hd = ImageDraw.Draw(hl)
        cx = sz // 2
        for r in range(int(sz * 0.45), 0, -2):
            t = 1 - r / (sz * 0.45)
            c = (
                int(BG[0] + (BG_HI[0] - BG[0]) * t * 0.4),
                int(BG[1] + (BG_HI[1] - BG[1]) * t * 0.4),
                int(BG[2] + (BG_HI[2] - BG[2]) * t * 0.4),
                255,
            )
            hd.ellipse([cx - r, cx - r, cx + r, cx + r], fill=c)
        mask = Image.new("L", (sz, sz), 0)
        ImageDraw.Draw(mask).rounded_rectangle(
            [0, 0, sz - 1, sz - 1], radius=radius, fill=255
        )
        img.paste(hl, (0, 0), mask)

    # emblem (draw natively at this size)
    scale = sz / 32.0
    draw_emblem(d, sz, scale)

    # apply rounded mask to clean edges
    final = Image.new("RGBA", (sz, sz), (0, 0, 0, 0))
    mask = Image.new("L", (sz, sz), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, sz - 1, sz - 1], radius=radius, fill=255
    )
    final.paste(img, (0, 0), mask)
    return final


def main():
    base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    icons_dir = os.path.join(base, "src-tauri", "icons")
    os.makedirs(icons_dir, exist_ok=True)

    # save high-res source (for reference)
    source = make_icon(1024)
    source.save(os.path.join(icons_dir, "source.png"))

    # PNG variants — EACH rendered natively at its target size (no blur)
    sizes = {
        "16x16.png": 16,
        "24x24.png": 24,
        "32x32.png": 32,
        "48x48.png": 48,
        "64x64.png": 64,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "256x256.png": 256,
        "icon.png": 512,
        "Square150x150Logo.png": 150,
        "Square44x44Logo.png": 44,
        "StoreLogo.png": 50,
    }
    for name, sz in sizes.items():
        make_icon(sz).save(os.path.join(icons_dir, name))

    # ICO multi-size — Pillow generates each size from the 256 source
    ico_source = make_icon(256)
    ico_source.save(
        os.path.join(icons_dir, "icon.ico"),
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )

    print("icons generated (native render per size):")
    for f in sorted(os.listdir(icons_dir)):
        if f.endswith((".png", ".ico")):
            p = os.path.join(icons_dir, f)
            print(f"  {f:30s} {os.path.getsize(p):>8d} bytes")


if __name__ == "__main__":
    main()
