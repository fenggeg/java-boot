from PIL import Image

src = Image.open("D:/my-project/java-boot/src-tauri/icons/source.png").convert("RGBA")

base = "D:/my-project/java-boot/src-tauri/icons/"

# PNG variants — covers all Tauri-required sizes
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
    src.resize((sz, sz), Image.LANCZOS).save(base + name)

# ICO multi-size
ico_sizes = [(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
src.save(base + "icon.ico", sizes=ico_sizes)

print("all icons generated")
import os
for f in sorted(os.listdir(base)):
    if f.endswith((".png", ".ico")):
        p = base + f
        print(f, os.path.getsize(p), "bytes")
