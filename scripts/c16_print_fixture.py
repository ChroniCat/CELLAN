from openpyxl import Workbook
from openpyxl.styles import Alignment, Border, Font, PatternFill, Side
from openpyxl.utils import get_column_letter

out = r"F:\finance-native-sheets-ready\crates\core\tests\fixtures\c16\print.xlsx"
wb = Workbook()
ws = wb.active
ws.title = "打印"

thin = Side(style="thin", color="FF333333")
box = Border(left=thin, right=thin, top=thin, bottom=thin)

ws.merge_cells("A1:D1")
ws["A1"] = "2026 年第一季度销售汇总"
ws["A1"].font = Font(name="微软雅黑", size=14, bold=True)
ws["A1"].alignment = Alignment(horizontal="center", vertical="center")
ws.row_dimensions[1].height = 30

headers = ["产品", "数量", "单价", "金额"]
for i, h in enumerate(headers, start=1):
    c = ws.cell(row=2, column=i, value=h)
    c.font = Font(bold=True)
    c.alignment = Alignment(horizontal="center", vertical="center")
    c.fill = PatternFill("solid", fgColor="FFD9E1F2")
    c.border = box
ws.row_dimensions[2].height = 22

rows = [
    ("铅笔", 120, 1.5, "=B3*C3"),
    ("笔记本", 30, 12.0, "=B4*C4"),
    ("橡皮", 200, 0.8, "=B5*C5"),
]
for r, (name, qty, price, amount) in enumerate(rows, start=3):
    ws.cell(row=r, column=1, value=name).border = box
    ws.cell(row=r, column=2, value=qty).border = box
    ws.cell(row=r, column=3, value=price).border = box
    c = ws.cell(row=r, column=4, value=amount)
    c.border = box
    c.number_format = "#,##0.00"

# 右对齐数字 / 居中布尔 / 左对齐文本 / General
ws["F3"] = 1234.5
ws["F4"] = True
ws["F5"] = "左对齐文本"
ws["F6"] = 0.256
ws["F6"].number_format = "0.0%"
ws["F7"] = -987.654
ws["F7"].number_format = "[Red]-#,##0.00"
ws["F8"] = 1234567.891
ws["F8"].number_format = '¥#,##0.00'

# 换行 + 垂直居中 + 缩进
ws.merge_cells("A8:D9")
ws["A8"] = "这一格用来自动换行：打印与预览必须按同一个显示模型折行，不能各算一套。"
ws["A8"].alignment = Alignment(wrap_text=True, vertical="center", horizontal="left", indent=1)
ws["A8"].font = Font(size=10)
for rr in (8, 9):
    ws.row_dimensions[rr].height = 20

# 生成几十行，保证多页（顶端标题行每页重复）
for r in range(10, 61):
    ws.cell(row=r, column=1, value=f"行{r}")
    ws.cell(row=r, column=2, value=r)
    ws.cell(row=r, column=3, value=r * 0.5)
    ws.cell(row=r, column=4, value=f"=B{r}*C{r}")

ws.column_dimensions["A"].width = 18
ws.column_dimensions["B"].width = 10
ws.column_dimensions["C"].width = 10
ws.column_dimensions["D"].width = 14
ws.column_dimensions["E"].width = 3
ws.column_dimensions["F"].width = 16

ws.print_title_rows = "1:2"
ws.page_setup.orientation = "portrait"
ws.page_setup.paperSize = ws.PAPERSIZE_A4
ws.page_setup.scale = 100
ws.print_options.horizontalCentered = False
ws.oddHeader.center.text = "销售汇总 &P/&N"
ws.oddFooter.left.text = "内部资料"
ws.oddFooter.right.text = "第 &P 页"

wb.save(out)
print("SAVED", out)
