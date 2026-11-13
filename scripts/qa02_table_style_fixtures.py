#!/usr/bin/env python3
"""Generate the synthetic table-object / style / merge fixtures for QA-02.

These samples back the `table_object`, `style_merge` and `format_edit` core
tests.  They are built from scratch with openpyxl -- no user workbook is read
or overwritten -- and the script refuses to replace an existing file.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook
from openpyxl.styles import Alignment, Border, Font, PatternFill, Side
from openpyxl.worksheet.table import Table, TableStyleInfo


def table_object(out: Path) -> None:
    """One list object `Table1` on sheet 报价 covering A1:C4."""
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "报价"
    rows = [
        ("姓名", "部门", "金额"),
        ("张三", "研发", 500),
        ("李四", "财务", 300),
        ("王五", "市场", 50),
    ]
    for r, values in enumerate(rows, start=1):
        for c, value in enumerate(values, start=1):
            ws.cell(row=r, column=c, value=value)
    tbl = Table(displayName="Table1", ref="A1:C4")
    tbl.tableStyleInfo = TableStyleInfo(
        name="TableStyleMedium2",
        showFirstColumn=False,
        showLastColumn=False,
        showRowStripes=True,
        showColumnStripes=False,
    )
    ws.add_table(tbl)
    wb.save(out)


def styled(out: Path) -> None:
    """Two sheets: styled/merged report plus a second sheet with 3 merges."""
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "样式"

    title_font = Font(name="微软雅黑", size=18, bold=True, color="FFFFFFFF")
    title_fill = PatternFill(fill_type="solid", start_color="FF1F4E78", end_color="FF1F4E78")
    head_font = Font(name="微软雅黑", size=11, bold=True, color="FFFFFFFF")
    head_fill = PatternFill(fill_type="solid", start_color="FF2E75B6", end_color="FF2E75B6")
    thin = Side(style="thin", color="FFBFBFBF")

    ws.merge_cells("A1:F1")
    ws["A1"] = "季度销售汇报"
    ws["A1"].font = title_font
    ws["A1"].fill = title_fill
    ws["A1"].alignment = Alignment(horizontal="center", vertical="center")

    headers = ["产品", "数量", "单价", "金额", "占比", "备注"]
    for c, text in enumerate(headers, start=1):
        cell = ws.cell(row=2, column=c, value=text)
        cell.font = head_font
        cell.fill = head_fill
        cell.border = Border(left=thin, right=thin, top=thin, bottom=thin)
        cell.alignment = Alignment(horizontal="center")

    data = [
        ("产品A", 120, 10.5, 1260.0, 0.32, "华东"),
        ("产品B", 80, 22.0, 1760.0, 0.44, "华南"),
        ("产品C", 45, 8.25, 371.25, 0.09, "华北"),
        ("产品D", 60, 9.9, 594.0, 0.15, "西南"),
    ]
    fills = ["FFFFF2CC", "FFE2EFDA", "FFDDEBF7", "FFFCE4D6"]
    for i, row in enumerate(data):
        r = 3 + i
        for c, value in enumerate(row, start=1):
            cell = ws.cell(row=r, column=c, value=value)
            cell.fill = PatternFill(fill_type="solid", start_color=fills[i], end_color=fills[i])
            cell.border = Border(left=thin, right=thin, top=thin, bottom=thin)
        ws.cell(row=r, column=1).font = Font(italic=True)
        ws.cell(row=r, column=2).number_format = "#,##0"
        ws.cell(row=r, column=3).number_format = '"¥"#,##0.00'
        ws.cell(row=r, column=4).font = Font(bold=True)
        ws.cell(row=r, column=4).number_format = "#,##0.00"
        ws.cell(row=r, column=5).number_format = "0.0%"
        ws.cell(row=r, column=6).alignment = Alignment(wrap_text=True)

    ws.merge_cells("A8:C8")
    ws["A8"] = "合计"
    ws["A8"].font = Font(bold=True)
    ws["A8"].fill = PatternFill(fill_type="solid", start_color="FFD9E1F2", end_color="FFD9E1F2")
    ws["A8"].alignment = Alignment(horizontal="center")

    ws2 = wb.create_sheet("合并")
    for r, values in enumerate([("区划", "金额", "备注"), ("华东", 1200, "一季度"), ("华南", 980, "二季度")], start=1):
        for c, value in enumerate(values, start=1):
            ws2.cell(row=r, column=c, value=value)
    ws2.merge_cells("A1:C1")
    ws2.merge_cells("A3:B4")
    ws2.merge_cells("D2:E3")
    wb.save(out)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()
    out_dir = Path(args.out_dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    targets = {
        out_dir / "table_obj.xlsx": table_object,
        out_dir / "styled.xlsx": styled,
    }
    existing = [p for p in targets if p.exists()]
    if existing:
        raise SystemExit("refusing to overwrite existing file(s): " + ", ".join(map(str, existing)))
    for path, build in targets.items():
        build(path)
        print(path)


if __name__ == "__main__":
    main()
