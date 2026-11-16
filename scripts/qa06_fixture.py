"""QA-06.1：错误/空值/日期矩阵 —— 造夹具并列出待对拍公式。

夹具 sheet "S"（与 crates/core/tests/qa06_matrix.rs 内存里构造的完全相同）：
    A1 空        A2 "文本"    A3 5        A4 TRUE      A5 #N/A
    A6 ""        A7 0         A8 1.5      A9 45351     A10 "5"
    A11 #DIV/0!  A12 45000
    B1..B5 = 1,2,3,4,5
    C 列 = 待算公式
"""

import json
import os
import sys

CASES = [
    # ---- 错误传播 ----
    ("err", "=A5+1"),
    ("err", "=A11*2"),
    ("err", "=SUM(A5,1)"),
    ("err", '=IFERROR(A5+1,"e")'),
    ("err", "=IFNA(A5,0)"),
    ("err", "=ISERROR(A5)"),
    ("err", "=ISERR(A5)"),
    ("err", "=ISNA(A5)"),
    ("err", "=ERROR.TYPE(A5)"),
    ("err", "=1/0"),
    ("err", "=NA()"),
    ("err", '=VALUE("abc")'),
    ("err", '=A5&"x"'),
    ("err", "=COUNT(A5)"),
    ("err", "=ISERROR(1/0)"),
    ("err", '=IFERROR(1/0,"z")'),
    # ---- 空值 / 空字符串 ----
    ("blank", "=A1+1"),
    ("blank", '=A1&"x"'),
    ("blank", "=A1=0"),
    ("blank", '=A1=""'),
    ("blank", '=A6=""'),
    ("blank", "=ISBLANK(A1)"),
    ("blank", "=ISBLANK(A6)"),
    ("blank", "=COUNTBLANK(A1:A7)"),
    ("blank", "=SUM(A1:A4,A6:A10)"),
    ("blank", "=COUNTA(A1:A10)"),
    ("blank", "=COUNT(A1:A10)"),
    ("blank", "=A10+1"),
    ("blank", "=A10&1"),
    ("blank", "=TRUE+1"),
    ("blank", "=SUM(A4)"),
    ("blank", "=AVERAGE(A1:A4,A6:A10)"),
    ("blank", "=COUNTBLANK(B1:B5)"),
    # ---- 日期序号 / 1900 系统 ----
    ("date", "=DATE(2024,2,29)"),
    ("date", "=DAY(45351)"),
    ("date", "=YEAR(45351)"),
    ("date", "=MONTH(45351)"),
    ("date", "=DATE(1900,2,29)"),
    ("date", "=YEAR(60)"),
    ("date", "=DAY(60)"),
    ("date", "=EDATE(45351,1)"),
    ("date", "=EOMONTH(45351,0)"),
    ("date", '=DATEDIF(45351,45382,"d")'),
    ("date", '=TEXT(45351,"yyyy-mm-dd")'),
    ("date", "=WEEKDAY(45351,2)"),
    ("date", "=WEEKNUM(45351,2)"),
    ("date", "=A9+1"),
    ("date", "=DATE(2024,13,1)"),
    ("date", "=DAYS(45382,45351)"),
    ("date", "=DATEVALUE(\"2024-02-29\")"),
]

INPUTS = {
    "A1": None,
    "A2": "文本",
    "A3": 5,
    "A4": True,
    "A5": "#N/A",
    "A6": '=""',
    "A7": 0,
    "A8": 1.5,
    "A9": 45351,
    "A10": "5",
    "A11": "#DIV/0!",
    "A12": 45000,
    "B1": 1,
    "B2": 2,
    "B3": 3,
    "B4": 4,
    "B5": 5,
}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "work/qa06"
    os.makedirs(out_dir, exist_ok=True)

    json.dump(
        {"inputs": INPUTS, "cases": [{"group": g, "formula": f} for g, f in CASES]},
        open(os.path.join(out_dir, "cases.json"), "w", encoding="utf-8"),
        ensure_ascii=False,
        indent=1,
    )

    from openpyxl import Workbook

    wb = Workbook()
    ws = wb.active
    ws.title = "S"

    def set_cell(ref, v):
        if v is None:
            return
        if isinstance(v, str) and v.startswith("#"):
            ws[ref] = v
        elif isinstance(v, bool):
            ws[ref] = v
        elif isinstance(v, str) and ref in ("A2", "A10"):
            ws[ref] = v
            if ref == "A10":
                ws[ref].number_format = "@"
            else:
                ws[ref].number_format = "@"
        else:
            ws[ref] = v

    for ref, v in INPUTS.items():
        set_cell(ref, v)

    # C 列的公式由 Excel 自己解析写入（见 qa06_excel_probe.ps1），
    # 否则 openpyxl 直接写 IFNA / DAYS 这类函数会缺 _xlfn 前缀，Excel 认成 #NAME?。

    path = os.path.join(out_dir, "qa06.xlsx")
    wb.save(path)
    print(f"fixture {path}: {len(CASES)} 条公式")
    print(f"cases  {os.path.join(out_dir, 'cases.json')}")


if __name__ == "__main__":
    main()
