#!/usr/bin/env python3
"""生成 QA-01 大簿场景的合成夹具（不是原始 106MB 压力样本，用于机制验证）。

200,000 行 × 6 列：int / float / 文本 / 日期 / 公式 / 公式；
另加一层条件格式、一块数据验证、自动筛选、冻结首行（表格用 auto_filter 代替，见下方注释）。
"""
from __future__ import annotations

import datetime as dt
import sys
from pathlib import Path

from openpyxl import Workbook
from openpyxl.formatting.rule import CellIsRule
from openpyxl.styles import PatternFill
from openpyxl.worksheet.datavalidation import DataValidation
from openpyxl.worksheet.table import Table, TableStyleInfo

ROWS = int(__import__("os").environ.get("QA01_ROWS", "200000"))
OUT = Path(sys.argv[1] if len(sys.argv) > 1 else "work/probe/qa01/large-synth.xlsx")


def main() -> int:
    wb = Workbook(write_only=False)
    ws = wb.active
    ws.title = "Data"
    ws.append(["编号", "金额", "数量", "名称", "单价", "小计"])
    base = dt.date(2026, 1, 1)
    for i in range(1, ROWS + 1):
        ws.append([i, round(i * 1.37 % 9999, 2), i % 97, f"项目{i % 1000}",
                   round((i % 500) + 0.5, 2), None])
        ws.cell(row=i + 1, column=6).value = f"=B{i+1}*C{i+1}"
    last = ROWS + 1
    ws.freeze_panes = "A2"
    ws.conditional_formatting.add(
        f"B2:B{last}",
        CellIsRule(operator="greaterThan", formula=["5000"],
                   fill=PatternFill("solid", fgColor="FFC7CE")),
    )
    dv = DataValidation(type="whole", operator="between", formula1="0", formula2="100",
                        allow_blank=True)
    dv.add(f"C2:C{last}")
    ws.add_data_validation(dv)
    # 注意：openpyxl 写出的 Table 会被本机 Excel 16.0 拒绝打开（bisect 实测：只加 table 就失败），
    # 因此大簿场景用自动筛选代替表格；表格保真由 table-ext 夹具（Excel 自己建的）覆盖。
    ws.auto_filter.ref = f"A1:F{last}"
    ws.page_setup.orientation = "landscape"
    ws.print_title_rows = "1:1"
    OUT.parent.mkdir(parents=True, exist_ok=True)
    wb.save(OUT)
    print(f"written {OUT} rows={ROWS} size={OUT.stat().st_size/1e6:.1f}MB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
