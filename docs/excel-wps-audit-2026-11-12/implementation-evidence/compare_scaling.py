"""UI-29.1：同一逻辑布局在 100/125/150/200% 下，UIA 矩形必须按倍率缩放。

用法：先跑 uia_dump.ps1 得到 u29-<ppp>.txt，再跑本脚本。
判定：|w2 - w1*ppp| <= max(2.5, 4%*w1*ppp)，高同理；Text 节点放宽到 6%（字形取整）。
2.0 档受本机屏幕高度限制（窗口被裁到 1440 物理），网格类元素排除，单独列出。
"""
import sys

def load(path):
    rows = {}
    for ln in open(path, encoding='utf-8', errors='replace'):
        f = [x.strip() for x in ln.rstrip('\n').split('\t')]
        at = [x for x in f if x.startswith('at=')]
        if not at:
            continue
        try:
            x, y, w, h = [float(v) for v in at[0][3:].split(',')]
        except ValueError:
            continue
        rows.setdefault((f[0], f[1]), []).append((x, y, w, h))
    return rows

def main(base_path, cases):
    base = load(base_path)
    total_bad = 0
    for ppp_s in cases:
        ppp = float(ppp_s)
        cur = load(base_path.replace('1.0', ppp_s))
        common = [k for k in base if k in cur and len(base[k]) == 1 and len(cur[k]) == 1]
        ok, bad, skipped = 0, [], 0
        for k in common:
            _, _, w1, h1 = base[k][0]
            _, _, w2, h2 = cur[k][0]
            if w1 < 12 or h1 < 10:
                continue
            if ppp == 2.0 and k[0] in ('DataGrid', 'DataItem'):
                # 本机屏幕 2560×1440 放不下 2560×1600，窗口被裁，网格高度不可比
                skipped += 1
                continue
            # 文本节点是字形外框：字宽按整像素取整，2.0 档能差 3px（~5%），
            # 所以文本用 6% 容差；按钮/输入框/网格这类盒子仍按 4%。
            frac = 0.06 if k[0] == 'Text' else 0.04
            tol_w = max(2.5, frac * w1 * ppp)
            tol_h = max(2.5, frac * h1 * ppp)
            if abs(w2 - w1 * ppp) <= tol_w and abs(h2 - h1 * ppp) <= tol_h:
                ok += 1
            else:
                bad.append((k, w1, w2, round(w2 / (w1 * ppp), 3), h1, h2,
                            round(h2 / (h1 * ppp), 3)))
        total_bad += len(bad)
        print(f'ppp={ppp_s}: 可比 {ok + len(bad)}，达标 {ok}，偏离 {len(bad)}'
              + (f'，环境排除 {skipped}' if skipped else ''))
        for k, w1, w2, rw, h1, h2, rh in bad:
            print(f'   {k[0]}/{k[1]}: {w1}x{h1} -> {w2}x{h2} 比例 {rw}/{rh}')
    print('RESULT', 'PASS' if total_bad == 0 else f'FAIL({total_bad})')
    return 0 if total_bad == 0 else 1

if __name__ == '__main__':
    sys.exit(main(sys.argv[1], sys.argv[2:]))
