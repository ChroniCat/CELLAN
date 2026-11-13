#!/usr/bin/env python3
"""Generate the synthetic table/chart fixtures used by QA-02 refs & filter tests.

These samples are new workbooks built from literals; openpyxl is required.
Existing files are never overwritten.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook
from openpyxl.chart import BarChart, LineChart, Reference

ROWS = (
    ("姓名", "部门", "金额"),
    ("张三", "研发", 300),
    ("李四", "财务", 100),
    ("王五", "研发", 200),
    ("赵六", "市场", 50),
    ("钱七", "财务", 400),
)


def fill_table(ws) -> None:
    ws.title = "表格"
    for r, values in enumerate(ROWS, 1):
        for c, value in enumerate(values, 1):
            ws.cell(row=r, column=c, value=value)


def table(out: Path) -> None:
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    fill_table(ws)
    wb.save(out)


def chart(out: Path) -> None:
    """A table plus two openpyxl charts, emulating a workbook made elsewhere."""
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    fill_table(ws)

    data = Reference(ws, min_col=3, min_row=1, max_row=6)
    cats = Reference(ws, min_col=1, min_row=2, max_row=6)

    bar = BarChart()
    bar.title = "部门金额"
    bar.add_data(data, titles_from_data=True)
    bar.set_categories(cats)
    ws.add_chart(bar, "E2")

    line = LineChart()
    line.title = "金额走势"
    line.add_data(data, titles_from_data=True)
    line.set_categories(cats)
    ws.add_chart(line, "E20")

    wb.save(out)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()
    out_dir = Path(args.out_dir).resolve()
    targets = [out_dir / "table.xlsx", out_dir / "chart.xlsx"]
    for target in targets:
        if target.exists():
            raise SystemExit(f"refusing to overwrite existing file: {target}")
    out_dir.mkdir(parents=True, exist_ok=True)
    table(targets[0])
    chart(targets[1])
    for target in targets:
        print(target)


if __name__ == "__main__":
    main()
