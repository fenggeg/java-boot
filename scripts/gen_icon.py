"""Generate JavaBoot Launcher icons — pixel-perfect at every size.

For small sizes (<=48, the taskbar/titlebar range), the emblem is drawn
with INTEGER pixel coordinates directly at the target size — no scaling,
no sub-pixel anti-aliasing blur. Larger sizes use the 32-unit viewBox.
"""
from PIL import Image, ImageDraw
import os

# palette
BG = (10, 11, 13, 255)
BG_HI = (28, 32, 37, 255)
LIME = (163, 230, 53, 255)
LIME_DIM = (132, 204, 22, 255)


def draw_emblem_small(d, sz):
    """Pixel-perfect emblem for small sizes. All coordinates are integers."""
    # bracket: left bar + top bar (bold, 2px for 16, scale up for bigger)
    t = max(2, sz // 8)          # bracket thickness
    m = sz // 8                  # margin from edge
    # left vertical bar
    d.rectangle([m, m, m + t - 1, sz - 1 - m], fill=LIME_DIM)
    # top horizontal bar
    d.rectangle([m, m, sz - 1 - m, m + t - 1], fill=LIME_DIM)
    # bottom horizontal bar (close the bracket into a "[" shape on left)
    d.rectangle([m, sz - 1 - m - (t - 1), sz - 1 - m, sz - 1 - m], fill=LIME_DIM)

    # play triangle — bold, centered, integer vertices
    cx = (m + t + sz - 1 - m) // 2 + t // 2
    top_y = m + t + max(1, sz // 12)
    bot_y = sz - 1 - m - t - max(1, sz // 12)
    mid_y = (top_y + bot_y) // 2
    left_x = m + t + max(1, sz // 10)
    right_x = left_x + (bot_y - top_y) // 2
    d.polygon([(left_x, top_y), (right_x, mid_y), (left_x, bot_y)], fill=LIME)


def draw_emblem_large(d, sz):
    """Scaled emblem for sizes >= 64. viewBox = 32 units."""
    s = sz / 32.0

    def P(x, y):
        return (x * s, y * s)

    t = 3.2  # bold bracket thickness
    # left bar
    d.rectangle([P(4, 4), P(4 + t, 28)], fill=LIME_DIM)
    # top bar
    d.rectangle([P(4, 4), P(28, 4 + t)], fill=LIME_DIM)
    # bottom bar
    d.rectangle([P(4, 28 - t), P(28, 28)], fill=LIME_DIM)

    # play triangle
    d.polygon([P(12, 10), P(22, 16), P(12, 22)], fill=LIME)


def make_icon(sz):
    img = Image.new("RGBA", (sz, sz), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # rounded-square dark background
    radius = max(1, int(sz * 0.2))
    d.rounded_rectangle([0, 0, sz - 1, sz - 1], radius=radius, fill=BG)

    # subtle center highlight only for larger sizes (keeps small ones crisp)
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

    # emblem
    if sz <= 48:
        draw_emblem_small(d, sz)
    else:
        draw_emblem_large(d, sz)

    # clean rounded edges
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

    make_icon(1024).save(os.path.join(icons_dir, "source.png"))

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

    # ICO — render each size natively so all are crisp
    ico_imgs = [make_icon(s) for s in [16, 24, 32, 48, 64, 128, 256]]
    # Pillow ICO: save the largest with sizes param, it rescales internally.
    # To keep our native renders, concatenate manually.
    ico_imgs[-1].save(
        os.path.join(icons_dir, "icon.ico"),
        append_images=ico_imgs[:-1],
        sizes=[(im.width, im.height) for im in ico_imgs],
    )

    print("icons generated (pixel-perfect per size):")
    for f in sorted(os.listdir(icons_dir)):
        if f.endswith((".png", ".ico")):
            print(f"  {f:30s} {os.path.getsize(os.path.join(icons_dir, f)):>8d} bytes")


if __name__ == "__main__":
    main()
