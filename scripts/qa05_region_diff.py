#!/usr/bin/env python3
"""QA-05.3 区域差异比对（禁止只给全图相似度）。

用法：
    python scripts/qa05_region_diff.py REF.png CUR.png [--regions a,b,c] [--json]

两张图尺寸不同时先把 CUR 缩放到 REF 的尺寸（高 DPI 截图降采样）。
输出按区域拆分的差异指标，并按 qa05-visual-spec.md 里的门槛判定。
"""
from __future__ import annotations

import argparse
import sys

from PIL import Image, ImageChops, ImageStat

# 区域按窗口高度比例切（UI 上下结构固定），可被 --regions 覆盖
# 门槛来自 2026-11-16 首次测量（同一逻辑尺寸 1440×900，ppp 1.0 vs 1.25/1.5 的降采样差），
# 取两档最大值 + 约 20% 余量。这些差值几乎全部是文字抗锯齿，所以 text 区域门槛必然偏高；
# 结构性变化（控件消失、错位 ≥4px）会明显超过它们——见 qa05-visual-spec.md 的合成反例。
REGIONS = [
    ("title", 0.000, 0.055, 7.0, 8.5),
    ("ribbon", 0.055, 0.170, 7.0, 5.5),
    ("formula", 0.170, 0.215, 7.5, 6.5),
    ("grid", 0.215, 0.940, 6.0, 3.5),
    ("tabs", 0.940, 0.975, 2.5, 1.0),
    ("status", 0.975, 1.000, 16.0, 12.0),
]
# 门槛：mean_abs（0-255 平均绝对差）与 big%（差值>32 的像素占比，单位 %）


def load_pair(ref_path: str, cur_path: str) -> tuple[Image.Image, Image.Image]:
    ref = Image.open(ref_path).convert("L")
    cur = Image.open(cur_path).convert("L")
    if cur.size != ref.size:
        cur = cur.resize(ref.size, Image.Resampling.LANCZOS)
    return ref, cur


def region_stats(ref: Image.Image, cur: Image.Image, box) -> dict:
    a = ref.crop(box)
    b = cur.crop(box)
    diff = ImageChops.difference(a, b)
    stat = ImageStat.Stat(diff)
    mean = stat.mean[0]
    px = list(diff.tobytes())
    n = max(1, len(px))
    big = sum(1 for v in px if v > 32) * 100.0 / n
    px.sort()
    p99 = px[int(n * 0.99) - 1] if n > 1 else 0
    return {"mean": mean, "p99": p99, "big": big, "px": n}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("ref")
    ap.add_argument("cur")
    ap.add_argument("--regions", default="")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    names = [r.strip() for r in args.regions.split(",") if r.strip()]
    regions = [r for r in REGIONS if not names or r[0] in names]
    if not regions:
        print(f"unknown regions: {names}", file=sys.stderr)
        return 2

    ref, cur = load_pair(args.ref, args.cur)
    w, h = ref.size
    rows = []
    fail = 0
    for name, y0, y1, max_mean, max_big in regions:
        box = (0, int(h * y0), w, min(h, int(h * y1)))
        st = region_stats(ref, cur, box)
        ok = st["mean"] <= max_mean and st["big"] <= max_big
        if not ok:
            fail += 1
        rows.append((name, box, st, max_mean, max_big, ok))

    if args.json:
        import json

        print(
            json.dumps(
                [
                    {
                        "region": n,
                        "box": b,
                        "mean": round(s["mean"], 2),
                        "p99": s["p99"],
                        "big_pct": round(s["big"], 2),
                        "limit_mean": mm,
                        "limit_big": mb,
                        "ok": ok,
                    }
                    for n, b, s, mm, mb, ok in rows
                ],
                ensure_ascii=False,
            )
        )
    else:
        print(f"# ref={args.ref} cur={args.cur} size={w}x{h}")
        print(f"{'region':8s} {'box':28s} {'mean':>6s} {'p99':>4s} {'big%':>6s}  limit      verdict")
        for n, b, s, mm, mb, ok in rows:
            print(
                f"{n:8s} {str(b):28s} {s['mean']:6.2f} {s['p99']:4d} {s['big']:6.2f}  "
                f"<={mm:.1f}/{mb:.1f}  {'PASS' if ok else 'FAIL'}"
            )
    return 0 if fail == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
