#!/usr/bin/env python3
"""重建 QA-09 的两个夹具（仓库里 *.xlsx 被忽略，这里给可复现来源）。

uv run --with openpyxl python scripts/make_qa09_fixtures.py
"""
import os

from openpyxl import Workbook
from openpyxl.styles import Font
from openpyxl.worksheet.properties import PageSetupProperties

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "core", "tests", "fixtures", "qa09")
os.makedirs(OUT, exist_ok=True)


def hidden():
    wb = Workbook()
    ws = wb.active
    ws.title = "隐藏打印"
    ws["A1"] = "隐藏行列打印样本"
    ws["A1"].font = Font(bold=True, size=14)
    ws.merge_cells("A1:C1")
    for c, v in zip("ABC", ["可见列A", "隐藏列B", "可见列C"]):
        ws[f"{c}2"] = v
    rows = [
        ("行3可见", "B3隐藏列内容", "C3可见"),
        ("行4可见", "B4隐藏列内容", "C4可见"),
        ("行5隐藏行内容", "B5隐藏", "C5隐藏"),
        ("行6可见", "B6隐藏列内容", "C6可见"),
        ("行7筛选隐藏", "B7隐藏", "C7隐藏"),
        ("行8可见", "B8隐藏列内容", "C8可见"),
    ]
    for i, vals in enumerate(rows, start=3):
        for c, v in zip("ABC", vals):
            ws[f"{c}{i}"] = v
    ws.row_dimensions[5].hidden = True
    ws.row_dimensions[7].hidden = True
    ws.column_dimensions["B"].hidden = True
    ws.page_setup.paperSize = 9
    ws.print_title_rows = "1:2"
    ws.oddFooter.center.text = "第 &P 页 / &N"
    wb.save(os.path.join(OUT, "hidden.xlsx"))


def fit_width():
    wb = Workbook()
    ws = wb.active
    ws.title = "适应纸张"
    for c in range(1, 19):
        ws.cell(row=1, column=c, value=f"第{c}列标题")
    for r in range(2, 32):
        for c in range(1, 19):
            ws.cell(row=r, column=c, value=f"{r}-{c}")
    for c in range(1, 19):
        ws.column_dimensions[ws.cell(row=1, column=c).column_letter].width = 11
    ws.sheet_properties.pageSetUpPr = PageSetupProperties(fitToPage=True)
    ws.page_setup.fitToWidth = 1
    ws.page_setup.fitToHeight = 0
    ws.page_setup.paperSize = 9
    wb.save(os.path.join(OUT, "fit_width.xlsx"))


if __name__ == "__main__":
    hidden()
    fit_width()
    print("wrote", OUT)
