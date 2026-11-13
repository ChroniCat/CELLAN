#!/usr/bin/env python3
"""C07.1 命名列表 + 跨表/公式阈值夹具。"""
import sys
import zipfile
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
for cell, val in (("A1", 10), ("A2", 20), ("A3", 30)):
    ws[cell] = val


def add(dv, sqref):
    dv.add(sqref)
    ws.add_data_validation(dv)


add(DataValidation(type="list", formula1="=甲乙", allow_blank=True), "B2")
add(DataValidation(type="whole", operator="greaterThanOrEqual", formula1="=$A$1", allow_blank=True), "C2")
add(DataValidation(type="list", formula1="=列表!$A$1:$A$2", allow_blank=True), "D2")
add(DataValidation(type="list", formula1="=不存在名称", allow_blank=True), "E2")
# 混合引用：列绝对、行相对；范围基准是 F2
add(DataValidation(type="whole", operator="greaterThanOrEqual", formula1="=$A1", allow_blank=True), "F2:F3")
add(DataValidation(type="list", formula1='=FILTER(列表!$A$1:$A$2,列表!$A$1:$A$2<>"")', allow_blank=True), "G2")
add(DataValidation(type="list", formula1="='O''Brien'!$A$1:$A$2", allow_blank=True), "H2")
add(DataValidation(type="list", formula1="=本表名", allow_blank=True), "I2")

names = wb.create_sheet("列表")
names["A1"] = "甲"
names["A2"] = "乙"
ob = wb.create_sheet("O'Brien")
ob["A1"] = "丙"
ob["A2"] = "丁"
wb.defined_names.add(DefinedName("甲乙", attr_text="列表!$A$1:$A$2"))
wb.defined_names.add(DefinedName("本表名", attr_text="列表!$A$1"))
wb.defined_names.add(DefinedName("本表名", attr_text="列表!$A$2", localSheetId=0))
wb.save(out)
# openpyxl 的字典不能同时保持同名的工作簿级和本表级名称；手工补一个全局同名项，
# 用来验证解析时“本表级优先”。
tmp = out.with_suffix(".tmp.xlsx")
with zipfile.ZipFile(out, "r") as src, zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as dst:
    for item in src.infolist():
        data = src.read(item.filename)
        if item.filename == "xl/workbook.xml":
            xml = data.decode("utf-8")
            marker = '<definedNames>'
            assert marker in xml
            xml = xml.replace(
                marker,
                marker + '<definedName name="本表名">列表!$A$1</definedName>',
                1,
            )
            data = xml.encode("utf-8")
        dst.writestr(item, data)
tmp.replace(out)
print(out)
