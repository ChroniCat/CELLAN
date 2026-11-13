#!/usr/bin/env python3
"""Generate the synthetic CSE array-formula fixture for QA-02.1.

Requires openpyxl (the rest of the project's compatibility fixtures use Python
tooling already).  The output is a new file; no existing workbook is touched.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook
from openpyxl.worksheet.formula import ArrayFormula


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    out = Path(args.out).resolve()
    if out.exists():
        raise SystemExit(f"refusing to overwrite existing file: {out}")
    out.parent.mkdir(parents=True, exist_ok=True)
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "数组"
    for row, values in enumerate([(1, 2, 3), (4, 5, 6), (7, 8, 9)], start=2):
        for col, value in enumerate(values, start=1):
            ws.cell(row=row, column=col, value=value)
    ws["B2"] = ArrayFormula("B2:D2", "=SUM(A2:C2)")
    wb.save(out)
    print(out)


if __name__ == "__main__":
    main()
