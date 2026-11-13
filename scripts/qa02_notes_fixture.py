#!/usr/bin/env python3
"""Generate the synthetic comment fixture for QA-02.1 (openpyxl required)."""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook
from openpyxl.comments import Comment


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
    ws.title = "批注"
    for cell, value in {"A1": "姓名", "B1": "金额", "A2": "甲", "B2": 100, "A3": "乙", "B3": 200}.items():
        ws[cell] = value
    ws["B2"].comment = Comment("这行数据要复核", "张三")
    ws["A3"].comment = Comment("客户确认过\n金额无误", "李四")
    wb.save(out)
    print(out)


if __name__ == "__main__":
    main()
