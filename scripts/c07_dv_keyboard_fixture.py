"""C07.3 键盘下拉夹具：B2 是一条 4 项内联序列验证，C2 是纯文本格。"""
import sys

from openpyxl import Workbook
from openpyxl.worksheet.datavalidation import DataValidation

out = sys.argv[1]
wb = Workbook()
ws = wb.active
ws.title = "键盘"
ws["A1"] = "水果"
ws["A2"] = "（在 B2 按 Alt+↓）"
ws["C2"] = "普通格"
dv = DataValidation(
    type="list",
    formula1='"苹果,香蕉,橙子,葡萄"',
    allow_blank=False,
    showDropDown=False,
    showErrorMessage=True,
    showInputMessage=True,
)
dv.promptTitle = "水果"
dv.prompt = "从下拉里选一个"
dv.errorTitle = "不在列表里"
dv.error = "只能选苹果/香蕉/橙子/葡萄"
ws.add_data_validation(dv)
dv.add("B2")
wb.save(out)
print("wrote", out)
