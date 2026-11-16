"""QA-06.2 夹具：动态数组/溢出与数组运算。

sheet "S"：
    B1:B5 = 3,1,2,5,4        C1:C5 = b,a,c,e,d
    公式锚点放在 E1/E10/E20/... 这些列，每个块之间留足 9 行，互不覆盖。
溢出块由 Excel 的 SpillingToRange 读出，见 scripts/qa06_array_probe.ps1。
"""

import os
import sys

CASES = [
    ("=SEQUENCE(3,2)", 1),
    ("=SEQUENCE(2,3,10,5)", 10),
    ("=SORT(B1:B5)", 20),
    ("=SORTBY(B1:B5,C1:C5)", 30),
    ("=FILTER(B1:B5,B1:B5>2)", 40),
    ("=TAKE(B1:B5,2)", 50),
    ("=DROP(B1:B5,3)", 60),
    ("=TRANSPOSE(B1:B5)", 70),
    ("=HSTACK(B1:B2,C1:C2)", 80),
    ("=VSTACK(B1:B2,C1:C2)", 90),
    ("=B1:B5*2", 100),
    ("=UNIQUE(B1:B5)", 110),
]


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "work/qa06a"
    os.makedirs(out, exist_ok=True)

    from openpyxl import Workbook

    wb = Workbook()
    ws = wb.active
    ws.title = "S"
    for i, v in enumerate([3, 1, 2, 5, 4], start=1):
        ws[f"B{i}"] = v
    for i, v in enumerate(["b", "a", "c", "e", "d"], start=1):
        ws[f"C{i}"] = v
        ws[f"C{i}"].number_format = "@"
    # 公式留给 Excel 写（见 ps1），这里只写锚点表
    wb.save(os.path.join(out, "qa06a.xlsx"))

    with open(os.path.join(out, "anchor.tsv"), "w", encoding="utf-8", newline="\n") as f:
        for formula, row in CASES:
            f.write(f"E{row}\t{formula}\n")
    print(f"夹具 {out}/qa06a.xlsx，{len(CASES)} 个数组公式锚点")


if __name__ == "__main__":
    main()
