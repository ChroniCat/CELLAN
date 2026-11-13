#!/usr/bin/env python3
"""C07.2 错误样式/输入提示夹具：停止、警告、信息三条规则 + 提示文字。"""
import sys
from pathlib import Path
from openpyxl import Workbook
from openpyxl.worksheet.datavalidation import DataValidation

out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("crates/core/tests/fixtures/c07/alerts.xlsx")
if out.exists():
    print(f"refusing to overwrite {out}", file=sys.stderr)
    raise SystemExit(2)
out.parent.mkdir(parents=True, exist_ok=True)
wb = Workbook()
ws = wb.active
assert ws is not None
ws.title = "输入"
ws["A1"] = "人数"


def add(dv, sqref):
    ws.add_data_validation(dv)
    dv.add(sqref)


add(
    DataValidation(
        type="whole",
        operator="between",
        formula1="1",
        formula2="10",
        allow_blank=True,
        showErrorMessage=True,
        showInputMessage=True,
        errorStyle="stop",
        errorTitle="停止：超出范围",
        error="只能填 1-10，确定后回到编辑框",
        promptTitle="人数",
        prompt="请输入 1-10 的整数",
    ),
    "B2",
)
add(
    DataValidation(
        type="whole",
        operator="between",
        formula1="1",
        formula2="10",
        allow_blank=True,
        showErrorMessage=True,
        showInputMessage=False,
        errorStyle="warning",
        errorTitle="警告：超出范围",
        error="可以继续，但请确认这个值",
    ),
    "C2",
)
add(
    DataValidation(
        type="whole",
        operator="between",
        formula1="1",
        formula2="10",
        allow_blank=True,
        showErrorMessage=True,
        showInputMessage=True,
        errorStyle="information",
        errorTitle="信息：超出范围",
        error="这个值不在建议范围内",
        promptTitle="说明",
        prompt="1-10 只是建议",
    ),
    "D2",
)
add(
    DataValidation(
        type="whole",
        operator="between",
        formula1="1",
        formula2="10",
        allow_blank=True,
        showErrorMessage=False,
        showInputMessage=False,
        errorTitle="不该弹出",
        error="未启用错误警告",
    ),
    "E2",
)
wb.save(out)
print(out)
