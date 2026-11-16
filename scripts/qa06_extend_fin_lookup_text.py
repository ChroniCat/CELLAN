"""QA-06.3：给夹具补财务 / 查找 / 文本三组，追加到 cases.json 与 qa06.xlsx。

查找表：D1:E5 = k1..k5 / 10,20,30,40,50；F1:F6 = -1000,300,400,500,200,100（现金流）
文本：H1="  Hello World  "，H2="Sheet1"，H3="a,b,c"，H4="中文ABC123"
"""

import json
import sys

EXTRA_CASES = [
    # ---- 财务 ----
    ("fin", "=NPV(0.1,F2:F6)+F1"),
    ("fin", "=IRR(F1:F6)"),
    ("fin", "=PMT(0.05/12,360,-300000)"),
    ("fin", "=PV(0.05,10,-100)"),
    ("fin", "=FV(0.05,10,-100)"),
    ("fin", "=RATE(10,-100,800)"),
    ("fin", "=NPER(0.05,-100,800)"),
    ("fin", "=IPMT(0.05/12,1,360,-300000)"),
    ("fin", "=PPMT(0.05/12,1,360,-300000)"),
    ("fin", "=SLN(10000,1000,9)"),
    ("fin", "=SYD(10000,1000,9,1)"),
    ("fin", "=DDB(10000,1000,9,1)"),
    ("fin", "=EFFECT(0.1,12)"),
    ("fin", "=NOMINAL(0.1,12)"),
    ("fin", "=CUMIPMT(0.05/12,360,-300000,1,12,0)"),
    ("fin", "=XIRR(F1:F6,DATE(2024,1,1)+{0;31;59;90;120;151})"),
    # ---- 查找 ----
    ("lookup", "=VLOOKUP(\"k3\",D1:E5,2,FALSE)"),
    ("lookup", "=VLOOKUP(\"k9\",D1:E5,2,FALSE)"),
    ("lookup", "=VLOOKUP(3.5,{1,10;2,20;3,30;4,40},2,TRUE)"),
    ("lookup", "=XLOOKUP(\"k4\",D1:D5,E1:E5)"),
    ("lookup", "=XLOOKUP(\"k9\",D1:D5,E1:E5,\"none\")"),
    ("lookup", "=MATCH(\"k2\",D1:D5,0)"),
    ("lookup", "=MATCH(\"k9\",D1:D5,0)"),
    ("lookup", "=INDEX(E1:E5,3)"),
    ("lookup", "=INDEX(D1:E5,2,2)"),
    ("lookup", "=LOOKUP(\"k3\",D1:D5,E1:E5)"),
    ("lookup", "=HLOOKUP(2,{1,2,3;10,20,30},2,FALSE)"),
    ("lookup", "=CHOOSE(2,\"a\",\"b\",\"c\")"),
    ("lookup", "=INDIRECT(\"E3\")"),
    ("lookup", "=OFFSET(E1,2,0)"),
    # ---- 文本 ----
    ("text", "=LEFT(H2,5)"),
    ("text", "=RIGHT(H2,1)"),
    ("text", "=MID(H2,6,1)"),
    ("text", "=LEN(H1)"),
    ("text", "=TRIM(H1)"),
    ("text", "=UPPER(H2)"),
    ("text", "=LOWER(\"ABC\")"),
    ("text", "=PROPER(\"hello world\")"),
    ("text", "=SUBSTITUTE(H3,\",\",\";\")"),
    ("text", "=FIND(\"W\",H1)"),
    ("text", "=SEARCH(\"w\",H1)"),
    ("text", "=REPLACE(H2,6,1,\"9\")"),
    ("text", "=TEXTJOIN(\"-\",TRUE,\"a\",\"\",\"b\")"),
    ("text", "=CONCAT(\"a\",1,TRUE)"),
    ("text", "=VALUE(\"1,234.5\")"),
    ("text", '=TEXT(1234.567,"#,##0.00")'),
    ("text", '=TEXT(0.256,"0.0%")'),
    ("text", '=TEXT(45351,"yyyy年m月d日")'),
    ("text", "=EXACT(\"a\",\"A\")"),
    ("text", "=LEN(H4)"),
    ("text", "=MID(H4,3,3)"),
    ("text", "=UNICODE(LEFT(H4,1))"),
]

EXTRA_INPUTS = {
    "D1": "k1", "E1": 10,
    "D2": "k2", "E2": 20,
    "D3": "k3", "E3": 30,
    "D4": "k4", "E4": 40,
    "D5": "k5", "E5": 50,
    "F1": -1000, "F2": 300, "F3": 400, "F4": 500, "F5": 200, "F6": 100,
    "H1": "  Hello World  ",
    "H2": "Sheet1",
    "H3": "a,b,c",
    "H4": "中文ABC123",
}


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
    data["inputs"].update(EXTRA_INPUTS)
    json.dump(data, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=1)

    from openpyxl import Workbook

    wb = Workbook()
    ws = wb.active
    ws.title = "S"
    for ref, v in data["inputs"].items():
        if v is None or (isinstance(v, str) and v.startswith("#")):
            continue
        if ref in ("A2", "A10", "D1", "D2", "D3", "D4", "D5", "H1", "H2", "H3", "H4"):
            ws[ref] = v
            ws[ref].number_format = "@"
        else:
            ws[ref] = v
    # A6 是真空字符串格，用公式表达
    ws["A6"] = '=""'
    # A5/A11 是错误值：用公式造出来，openpyxl 直接写 "#N/A" 会变成文本
    ws["A5"] = "=NA()"
    ws["A11"] = "=1/0"
    wb.save(f"{d}/qa06.xlsx")
    print(f"追加 {added} 条，合计 {len(data['cases'])} 条；夹具已重建")


if __name__ == "__main__":
    main()
