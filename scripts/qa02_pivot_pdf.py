#!/usr/bin/env python3
"""Generate the synthetic pivot/PDF fixture used by QA-02 core tests.

The workbook is a new, hand-built sample: one sheet named 「表格」 with the
columns 姓名 / 部门 / 金额 and five data rows.  The department totals are the
values asserted by `pivot_file.rs` and `pivot_real.rs`:

    研发 300 + 200 = 500
    销售 100 + 400 = 500
    行政 50

`pdf_export.rs` prints the same sheet.  openpyxl is required; no user
workbook is opened or modified.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook

ROWS = [
    ("姓名", "部门", "金额"),
    ("张三", "研发", 300),
    ("李四", "销售", 100),
    ("王五", "研发", 200),
    ("赵六", "行政", 50),
    ("钱七", "销售", 400),
]


def table(out: Path) -> None:
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "表格"
    for r, values in enumerate(ROWS, 1):
        for c, value in enumerate(values, 1):
            ws.cell(row=r, column=c, value=value)
    wb.save(out)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()
    out_dir = Path(args.out_dir).resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"output directory is not empty: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    target = out_dir / "table.xlsx"
    if target.exists():
        raise SystemExit(f"refusing to overwrite existing file: {target}")
    table(target)
    print(target)


if __name__ == "__main__":
    main()
