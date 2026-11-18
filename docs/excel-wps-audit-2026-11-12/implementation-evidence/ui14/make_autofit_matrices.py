"""生成 AutoFit 文本口径矩阵：ASCII 1..12 字符 × 字号 8..24；样式 3 串 × 17 字号 × 5 样式。

用法：uv run --with openpyxl python make_autofit_matrices.py
生成 text_ascii_matrix.xlsx / style_file_matrix.xlsx，再由
excel_text_ascii_matrix.ps1 / excel_style_file_matrix.ps1 用 Excel AutoFit 采集。
"""
from openpyxl import Workbook
from openpyxl.styles import Alignment, Font

ASCII = ["1" * n for n in range(1, 13)]
CJK = ["中", "中文", "中文字", "中文文字", "中文文字中"]
STYLES = [
    ("regular", False, 0),
    ("bold", True, 0),
    ("indent1", False, 1),
    ("indent2", False, 2),
    ("bold_indent1", True, 1),
]


def add_cell(ws, col, value, size, bold=False, indent=0):
    cell = ws.cell(row=1, column=col, value=value)
    cell.font = Font(name="等线", size=size, bold=bold)
    if indent:
        cell.alignment = Alignment(indent=indent)


def make_text_ascii(path):
    wb = Workbook()
    ws = wb.active
    col = 0
    for value in ASCII:
        for size in range(8, 25):
            col += 1
            add_cell(ws, col, value, size)
    wb.save(path)
    return col


def make_styles(path):
    wb = Workbook()
    ws = wb.active
    col = 0
    strings = ["1", CJK[-1], ASCII[-1]]
    for value in strings:
        for size in range(8, 25):
            for _, bold, indent in STYLES:
                col += 1
                add_cell(ws, col, value, size, bold, indent)
    wb.save(path)
    return col


if __name__ == "__main__":
    print("text_ascii_matrix cols", make_text_ascii("text_ascii_matrix.xlsx"))
    print("style_file_matrix cols", make_styles("style_file_matrix.xlsx"))
