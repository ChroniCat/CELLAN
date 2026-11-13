#!/usr/bin/env python3
"""C16.1 合成打印夹具：日期/百分比/货币/负数红字。"""
import sys
from pathlib import Path
from openpyxl import Workbook
from openpyxl.styles import Font

out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("crates/core/tests/fixtures/c16/number_formats.xlsx")
if out.exists():
    print(f"refusing to overwrite {out}", file=sys.stderr)
    raise SystemExit(2)
out.parent.mkdir(parents=True, exist_ok=True)
wb = Workbook()
ws = wb.active
assert ws is not None
ws.title = "打印"
ws["A1"] = "日期"
ws["B1"] = "2026-01-15"
ws["B1"].number_format = "yyyy-mm-dd"
ws["A2"] = "百分比"
ws["B2"] = 0.15
ws["B2"].number_format = "0%"
ws["A3"] = "货币"
ws["B3"] = 1234.5
ws["B3"].number_format = "¥#,##0.00"
ws["A4"] = "负数"
ws["B4"] = -123.45
ws["B4"].number_format = "#,##0.00;[Red]-#,##0.00"
for cell in ("A1", "A2", "A3", "A4"):
    ws[cell].font = Font(bold=True)
ws.column_dimensions["A"].width = 14
ws.column_dimensions["B"].width = 20
wb.save(out)
print(out)
