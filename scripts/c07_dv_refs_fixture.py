#!/usr/bin/env python3
"""C07.1 命名列表 + 公式阈值夹具。"""
import sys
from pathlib import Path
from openpyxl import Workbook
from openpyxl.workbook.defined_name import DefinedName
from openpyxl.worksheet.datavalidation import DataValidation

out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("crates/core/tests/fixtures/c07/refs.xlsx")
if out.exists():
    print(f"refusing to overwrite {out}", file=sys.stderr)
    raise SystemExit(2)
out.parent.mkdir(parents=True, exist_ok=True)
wb = Workbook()
ws = wb.active
assert ws is not None
ws.title = "输入"
ws["A1"] = 1
dv_list = DataValidation(type="list", formula1="=甲乙", allow_blank=True)
dv_list.add("B2")
ws.add_data_validation(dv_list)
dv_num = DataValidation(type="whole", operator="greaterThanOrEqual", formula1="=$A$1", allow_blank=True)
dv_num.add("C2")
ws.add_data_validation(dv_num)
names = wb.create_sheet("列表")
names["A1"] = "甲"
names["A2"] = "乙"
wb.defined_names.add(DefinedName("甲乙", attr_text="列表!$A$1:$A$2"))
wb.save(out)
print(out)
