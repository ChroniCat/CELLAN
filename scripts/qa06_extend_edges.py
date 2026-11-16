"""QA-06.3：补「查找边界」与「TEXT 分段/区域语言」两组用例。

查找边界：通配符 * ?、错误值当查找键、近似匹配遇到重复值。
TEXT：四段式（正;负;零;文本）、[Red]/转义引号、mmm/ddd/mmmm/dddd 区域语言。

夹具沿用 qa06_extend_fin_lookup_text.py 建好的 D1:E5 / F1:F6 / H1:H4 / A5(#N/A) / A11(#DIV/0!)。
"""

import json
import sys

EXTRA_CASES = [
    # ---- 查找边界：通配符 ----
    ("lookup", '=VLOOKUP("k*",D1:E5,2,FALSE)'),
    ("lookup", '=VLOOKUP("?3",D1:E5,2,FALSE)'),
    ("lookup", '=VLOOKUP("k9*",D1:E5,2,FALSE)'),
    ("lookup", '=MATCH("k*",D1:D5,0)'),
    ("lookup", '=MATCH("?2",D1:D5,0)'),
    ("lookup", '=XLOOKUP("k*",D1:D5,E1:E5,,2)'),
    # ---- 查找边界：错误值当查找键 ----
    ("lookup", "=VLOOKUP(A5,D1:E5,2,FALSE)"),
    ("lookup", "=MATCH(A5,D1:D5,0)"),
    ("lookup", "=VLOOKUP(A11,D1:E5,2,FALSE)"),
    # ---- 查找边界：近似匹配遇重复值（Excel 取最后一个 <= 的） ----
    ("lookup", "=VLOOKUP(3,{1,10;3,30;3,33;5,50},2,TRUE)"),
    ("lookup", "=LOOKUP(3,{1,3,3,5},{10,30,33,50})"),
    ("lookup", "=MATCH(3,{1,3,3,5},1)"),
    ("lookup", "=VLOOKUP(3.5,{1,10;3,30;3,33;5,50},2,TRUE)"),
    # ---- TEXT：分段与转义 ----
    ("text", '=TEXT(0,"0;-0;""zero"";""text: ""@")'),
    ("text", '=TEXT(0,"0.00;-0.00;""零""")'),
    ("text", '=TEXT(-5,"0.00;-0.00;""零""")'),
    ("text", '=TEXT("abc","General;General;General;""T:""@")'),
    ("text", '=TEXT(1234.5,"#,##0.00")'),
    # ---- TEXT：区域语言的月份/星期名 ----
    ("text", '=TEXT(DATE(2024,3,15),"mmm ddd")'),
    ("text", '=TEXT(DATE(2024,3,15),"mmmm dddd")'),
    ("text", '=TEXT(DATE(2024,3,15),"yyyy-mm-dd;@")'),
    ("text", '=TEXT(DATE(2024,12,1),"mmm")'),
]


def main():
    d = sys.argv[1] if len(sys.argv) > 1 else "work/qa06"
    path = f"{d}/cases.json"
    data = json.load(open(path, encoding="utf-8"))
    have = {c["formula"] for c in data["cases"]}
    added = 0
    for g, f in EXTRA_CASES:
        if f in have:
            continue
        data["cases"].append({"group": g, "formula": f})
        added += 1
    json.dump(data, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=1)

    from openpyxl import Workbook

    wb = Workbook()
    ws = wb.active
    ws.title = "S"
    for ref, v in data["inputs"].items():
        if v is None or (isinstance(v, str) and v.startswith("#")):
            continue
        ws[ref] = v
        if ref in ("A2", "A10", "D1", "D2", "D3", "D4", "D5", "H1", "H2", "H3", "H4"):
            ws[ref].number_format = "@"
    ws["A6"] = '=""'
    ws["A5"] = "=NA()"
    ws["A11"] = "=1/0"
    wb.save(f"{d}/qa06.xlsx")
    print(f"追加 {added} 条，合计 {len(data['cases'])} 条；夹具已重建")


if __name__ == "__main__":
    main()
