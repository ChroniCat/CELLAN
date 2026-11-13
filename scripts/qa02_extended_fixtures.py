#!/usr/bin/env python3
"""Generate the small table/page/freeze fixtures used by QA-02 core tests.

These are synthetic samples only.  openpyxl is required; output files are new
and are intentionally not created from any user workbook.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook


def table(out: Path) -> None:
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "表格"
    rows = [("姓名", "部门", "金额"), ("张三", "研发", 300), ("李四", "财务", 100),
            ("王五", "研发", 200), ("赵六", "市场", 50), ("钱七", "财务", 400)]
    for r, values in enumerate(rows, 1):
        for c, value in enumerate(values, 1):
            ws.cell(row=r, column=c, value=value)
    wb.save(out)


def page(out: Path) -> None:
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "页面"
    for r in range(1, 41):
        for c in range(1, 9):
            ws.cell(row=r, column=c, value=r * c)
    ws.page_setup.orientation = "landscape"
    ws.page_setup.paperSize = 8
    ws.page_setup.scale = 85
    ws.page_margins.left = 0.5
    ws.print_options.gridLines = True
    ws.print_area = "A1:H40"
    ws.print_title_rows = "1:2"
    wb.save(out)


def freeze(out: Path) -> None:
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "冻结"
    for r in range(1, 11):
        for c in range(1, 6):
            ws.cell(row=r, column=c, value=r * c)
    ws.freeze_panes = "C2"
    wb.save(out)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()
    out_dir = Path(args.out_dir).resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"output directory is not empty: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    table(out_dir / "table.xlsx")
    page(out_dir / "page.xlsx")
    freeze(out_dir / "freeze.xlsx")
    print(out_dir)


if __name__ == "__main__":
    main()
