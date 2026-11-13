"""C07.1 动态列表夹具：OFFSET / INDIRECT / FILTER 非空 / 名称 / 括号表名 / 相对来源平移。"""
import sys

from openpyxl import Workbook
from openpyxl.workbook.defined_name import DefinedName
from openpyxl.worksheet.datavalidation import DataValidation

out = sys.argv[1]
wb = Workbook()
ws = wb.active
ws.title = "输入"
for i, v in enumerate([10, 20, 30], start=1):
    ws.cell(row=i, column=1, value=v)

lst = wb.create_sheet("列表")
for i, t in enumerate(["甲", "乙", "丙"], start=1):
    lst.cell(row=i, column=1, value=t)

paren = wb.create_sheet("预算(2026)")
paren["A1"] = "甲"
paren["A2"] = "乙"

def add(ws_, formula, ref):
    dv = DataValidation(type="list", formula1=formula, allow_blank=True, showErrorMessage=True)
    ws_.add_data_validation(dv)
    dv.add(ref)

add(ws, "=OFFSET(列表!$A$1,0,0,COUNTA(列表!$A$1:$A$3),1)", "B2")
add(ws, '=INDIRECT("列表!A1:A2")', "C2")
add(ws, '=FILTER(列表!$A$1:$A$4,列表!$A$1:$A$4<>"")', "D2")
add(ws, "=$A1:$A2", "E2:E3")
add(ws, "='预算(2026)'!$A$1:$A$2", "F2")
add(ws, "=动态", "G2")

wb.defined_names["动态"] = DefinedName(
    "动态", attr_text="OFFSET(列表!$A$1,0,0,COUNTA(列表!$A$1:$A$3),1)"
)
wb.save(out)
print("wrote", out)
