"""C16 归一化逐页比对：先按内容框反解缩放，再比 token 坐标/文本集合/分页末行。

用法: python c16_pdf_normalized.py ours.pdf excel.pdf
"""
import re, sys
import numpy as np
import pypdfium2 as pdfium

TOKENS = ["2026 年第一季度销售汇总", "产品", "铅笔", "¥1,234,567.89", "--987.65", "25.6%", "180.00"]

def pages(path):
    doc = pdfium.PdfDocument(path)
    out = []
    for i in range(len(doc)):
        page = doc[i]
        img = page.render(scale=2.0).to_pil().convert("L")
        arr = np.asarray(img)
        ys, xs = np.where(arr < 235)
        bbox = (xs.min(), ys.min(), xs.max(), ys.max()) if len(xs) else (0, 0, 0, 0)
        tp = page.get_textpage()
        out.append({"size": page.get_size(), "bbox": bbox, "text": tp.get_text_range(), "tp": tp})
    return out

def token_pos(pg, tok):
    k = pg["text"].find(tok)
    if k < 0:
        return None
    l, b, r, t = pg["tp"].get_charbox(k)
    return (l, b)

def main(ours, excel):
    A, B = pages(ours), pages(excel)
    print(f"pages ours={len(A)} excel={len(B)}")
    for i in range(min(len(A), len(B))):
        a, b = A[i], B[i]
        # 用内容框宽度/高度反解缩放（同一页内容）
        aw = max(a["bbox"][2] - a["bbox"][0], 1); bw = max(b["bbox"][2] - b["bbox"][0], 1)
        ah = max(a["bbox"][3] - a["bbox"][1], 1); bh = max(b["bbox"][3] - b["bbox"][1], 1)
        sx, sy = bw / aw, bh / ah
        sa = sorted(re.sub(r"\s+", "", a["text"]))
        sb = sorted(re.sub(r"\s+", "", b["text"]))
        same_set = "".join(sa) == "".join(sb)
        ra = re.findall(r"行(\d+)", a["text"]); rb = re.findall(r"行(\d+)", b["text"])
        print(f"p{i+1} scale=({sx:.4f},{sy:.4f}) textset_same={same_set} "
              f"last_row ours={ra[-1] if ra else '-'} excel={rb[-1] if rb else '-'}")
        if not same_set:
            only_a = set(sa) - set(sb); only_b = set(sb) - set(sa)
            print(f"   only_ours={''.join(sorted(only_a))[:60]!r} only_excel={''.join(sorted(only_b))[:60]!r}")
        for tok in TOKENS:
            pa, pb = token_pos(a, tok), token_pos(b, tok)
            if pa is None or pb is None:
                print(f"   {tok!r}: ours={pa} excel={pb}")
                continue
            # 以各自内容框左上为原点，按 x 缩放后比较
            ax = (pa[0] - a["bbox"][0]) * sx; bx = pb[0] - b["bbox"][0]
            ay = (pa[1] - a["bbox"][1]) * sy; by = pb[1] - b["bbox"][1]
            print(f"   {tok!r}: dx={ax-bx:+.1f} dy={ay-by:+.1f}")

main(sys.argv[1], sys.argv[2])
