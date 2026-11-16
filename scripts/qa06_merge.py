"""把 Excel / WPS 的探针输出合并成对拍矩阵。

输出 crates/core/tests/fixtures/qa06/matrix.tsv：
    group<TAB>formula<TAB>excel_text<TAB>excel_value2<TAB>wps_text
并打印两边不一致的行（不影响矩阵，只做记录）。
"""

import os
import sys

src = sys.argv[1] if len(sys.argv) > 1 else "work/qa06"
dst = (
    sys.argv[2]
    if len(sys.argv) > 2
    else "crates/core/tests/fixtures/qa06/matrix.tsv"
)


def load(name):
    p = os.path.join(src, name)
    if not os.path.exists(p):
        return None
    rows = []
    with open(p, encoding="utf-8-sig") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if not line:
                continue
            rows.append(line.split("\t"))
    return rows


excel = load("excel.tsv")
assert excel, "excel.tsv 缺失"
wps = load("wps.tsv")

os.makedirs(os.path.dirname(dst), exist_ok=True)
out = ["# QA-06.1 group / formula / Excel 显示值 / Excel Value2 / WPS 显示值"]
diffs = []
for i, row in enumerate(excel):
    group, formula, text, v2 = (row + [""] * 4)[:4]
    wtext = ""
    if wps and i < len(wps):
        wtext = wps[i][2] if len(wps[i]) > 2 else ""
        if wtext != text:
            diffs.append(f"{formula}: Excel={text!r} WPS={wtext!r}")
    out.append("\t".join([group, formula, text, v2, wtext]))

open(dst, "w", encoding="utf-8", newline="\n").write("\n".join(out) + "\n")
print(f"矩阵 {dst}: {len(excel)} 行")
if diffs:
    print("Excel/WPS 差异：")
    for d in diffs:
        print("  ", d)
else:
    print("Excel 与 WPS 全部一致")
