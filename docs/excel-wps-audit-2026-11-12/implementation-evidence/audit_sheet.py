"""从截好的图标对照表里量每个格子的墨量，找空白格/糊满格。"""
import sys
from PIL import Image

ICON_NAMES = None

def main(path, size, count, cols=12):
    im = Image.open(path).convert('RGB')
    W, H = im.size
    cell_w = W // cols
    cell_h = int((H - 34) // 12)
    px = im.load()
    blank, heavy, rows = [], [], []
    for i in range(count):
        cx = (i % cols) * cell_w
        cy = 26 + (i // cols) * cell_h
        x0, y0 = cx + 8, cy + 8
        ink = 0
        for y in range(y0, min(y0 + size, H)):
            for x in range(x0, min(x0 + size, W)):
                r, g, b = px[x, y]
                if not (r > 245 and g > 245 and b > 245):
                    ink += 1
        ratio = ink / (size * size)
        rows.append((i, ratio))
        if ratio < 0.02:
            blank.append((i, round(ratio, 3)))
        if ratio > 0.92:
            heavy.append((i, round(ratio, 3)))
    print(f'{path}: {count} 格，空白 {len(blank)}，糊满 {len(heavy)}')
    print('  墨量分布 min/中位/max:',
          round(min(r for _, r in rows), 3),
          round(sorted(r for _, r in rows)[len(rows)//2], 3),
          round(max(r for _, r in rows), 3))
    if blank:
        print('  空白格:', blank[:10])
    if heavy:
        print('  糊满格:', heavy[:10])

if __name__ == '__main__':
    main(sys.argv[1], int(sys.argv[2]), int(sys.argv[3]))
