#!/usr/bin/env python3
"""Generate the QA-01 extended chart/pivot/print synthetic fixtures.

Uses openpyxl only for synthetic data and drawing objects.  Every output is a
new file under --out-dir; no user workbook is read or modified.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook
from openpyxl.chart import BarChart, Reference
from openpyxl.workbook.defined_name import DefinedName


def make_table() -> Workbook:
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "表格"
    rows = [("姓名", "部门", "金额"), ("张三", "研发", 300), ("李四", "销售", 100),
            ("王五", "研发", 200), ("赵六", "行政", 50), ("钱七", "销售", 400)]
    for r, values in enumerate(rows, 1):
        for c, value in enumerate(values, 1):
            ws.cell(row=r, column=c, value=value)
    return wb


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()
    out_dir = Path(args.out_dir).resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"output directory is not empty: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)

    # Pivot source / chart data.
    table = make_table()
    table.save(out_dir / "table.xlsx")

    # A real drawing object, so load/save exercises the chart package path.
    chart = make_table()
    ws = chart.active
    assert ws is not None
    bar = BarChart()
    bar.title = "按部门金额"
    data = Reference(ws, min_col=3, min_row=1, max_row=6)
    cats = Reference(ws, min_col=2, min_row=2, max_row=6)
    bar.add_data(data, titles_from_data=True)
    bar.set_categories(cats)
    ws.add_chart(bar, "E2")
    chart.save(out_dir / "chart.xlsx")

    # Print settings used by the page/PDF fixture.
    page = Workbook()
    ps = page.active
    assert ps is not None
    ps.title = "页面"
    for r in range(1, 41):
        for c in range(1, 9):
            ps.cell(row=r, column=c, value=r * c)
    ps.page_setup.orientation = "landscape"
    ps.page_setup.paperSize = 8
    ps.page_setup.scale = 85
    ps.page_margins.left = 0.5
    ps.print_options.gridLines = True
    ps.print_area = "A1:H40"
    ps.print_title_rows = "1:2"
    page.save(out_dir / "page.xlsx")

    # Named range + hidden row/column + frozen pane metadata.
    meta = Workbook()
    ms = meta.active
    assert ms is not None
    ms.title = "结构"
    for r in range(1, 11):
        for c in range(1, 6):
            ms.cell(row=r, column=c, value=r * c)
    ms.row_dimensions[3].hidden = True
    ms.column_dimensions["C"].hidden = True
    ms.freeze_panes = "C2"
    meta.defined_names["QA_RANGE"] = DefinedName("QA_RANGE", attr_text="'结构'!$A$1:$C$6")
    meta.save(out_dir / "metadata.xlsx")
    print(out_dir)


if __name__ == "__main__":
    main()
