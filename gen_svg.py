import vtracer
from PIL import Image
from collections import Counter

src = r'C:\Users\Administrator\.workbuddy\clipboard-images\clipboard-2026-09-04T19-18-20-421Z-9324d28b.jpg'
out = r'C:\Users\Administrator\WorkBuddy\2026-08-27-14-00-03\tinypic\src-tauri\icons\tinypic-logo.svg'

im = Image.open(src).convert('RGBA')
w, h = im.size
print('size', w, h, 'mode', im.mode)

cnt = Counter()
transparent = 0
for pixel in im.getdata():
    r, g, b, a = pixel
    if a < 128:
        transparent += 1
        continue
    cnt[(r // 32 * 32, g // 32 * 32, b // 32 * 32)] += 1
print('transparent pixels:', transparent, '/', w * h)
print('top opaque colors (quantized to 32-step):')
for c, n in cnt.most_common(10):
    print('  rgb', c, 'count', n)

vtracer.convert_image_to_svg_py(
    src,
    out,
    colormode='color',
    mode='spline',
    filter_speckle=4,
    color_precision=8,
    corner_threshold=60,
    length_threshold=10,
    path_precision=8,
)
import os
print('wrote', out, 'svg bytes', os.path.getsize(out))
