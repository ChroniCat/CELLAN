#!/usr/bin/env python3
"""Generate synthetic conditional-formatting and data-validation fixtures (QA-02).

Only openpyxl-built sample workbooks are produced; nothing is derived from a
user workbook and existing files are never overwritten.  The fixtures feed
crates/core/tests/conditional_format.rs and crates/core/tests/data_validation.rs.
"""
from __future__ import annotations

import argparse
from pathlib import Path

from openpyxl import Workbook
from openpyxl.formatting.rule import CellIsRule, ColorScaleRule, DataBarRule, FormulaRule, IconSetRule
from openpyxl.styles import Font, PatternFill
from openpyxl.worksheet.datavalidation import DataValidation


def _fill(rgb: str) -> PatternFill:
    # dxf 读入时代码取 <fill> 里第一个 <color>，fg/bg 都写成同一颜色更稳
    return PatternFill(start_color=rgb, end_color=rgb, fill_type="solid")


def cf(out: Path) -> None:
    """B 列大等于规则 + 数据条、C 列色阶、D 列公式规则。"""
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "条件"
    ws.append(["项目", "金额", "占比", "备注"])
    ws.append(["甲", 50, 10, "正常"])
    ws.append(["乙", 180, 20, "紧急"])
    ws.append(["丙", 320, 30, "很急"])
    ws.append(["丁", 60, 40, "正常"])

    # 金额 > 100 → 红底深红字（dxf 0）
    ws.conditional_formatting.add(
        "B2:B10",
        CellIsRule(
            operator="greaterThan",
            formula=["100"],
            fill=_fill("FFFFC7CE"),
            font=Font(color="FF9C0006"),
        ),
    )
    # 占比 3 色色阶（无 dxf）
    ws.conditional_formatting.add(
        "C2:C4",
        ColorScaleRule(
            start_type="min",
            start_color="FFF8696B",
            mid_type="percentile",
            mid_value=50,
            mid_color="FFFFEB84",
            end_type="max",
            end_color="FF63BE7B",
        ),
    )
    # 金额数据条（无 dxf）
    ws.conditional_formatting.add(
        "B2:B4",
        DataBarRule(start_type="min", end_type="max", color="FF638EC6", showValue=True),
    )
    # 备注含「急」→ 黄底（dxf 1）；公式相对块左上角 D2 书写，
    # 用 Excel「文本包含」实际生成的表达式
    ws.conditional_formatting.add(
        "D2:D5",
        FormulaRule(formula=['NOT(ISERROR(SEARCH("急",D2)))'], fill=_fill("FFFFEB9C")),
    )
    wb.save(out)


def icons(out: Path) -> None:
    """A 列红绿灯、B 列箭头，均为 3 级图标集。"""
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "图标"
    ws.append(["完成度", "趋势"])
    for value in (10, 55, 90):
        ws.append([value, value])
    ws.conditional_formatting.add(
        "A2:A4",
        IconSetRule("3TrafficLights1", "percent", [0, 33, 67]),
    )
    ws.conditional_formatting.add(
        "B2:B4",
        IconSetRule("3Arrows", "percent", [0, 33, 67]),
    )
    wb.save(out)


def dv(out: Path) -> None:
    """B2 整数 1..100，C2 序列 A,B,C,D。"""
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "验证"
    ws.append(["姓名", "人数", "等级"])
    ws.append(["张三", 50, "A"])
    ws.append(["李四", 80, "B"])

    whole = DataValidation(
        type="whole",
        operator="between",
        formula1="1",
        formula2="100",
        allow_blank=True,
        showErrorMessage=True,
        showInputMessage=True,
        errorTitle="人数超范围",
        error="只能填 1-100 的整数",
        promptTitle="人数",
        prompt="请输入 1-100 的整数",
    )
    whole.add("B2")
    ws.add_data_validation(whole)

    items = DataValidation(type="list", formula1='"A,B,C,D"', allow_blank=True)
    items.add("C2")
    ws.add_data_validation(items)
    wb.save(out)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()
    out_dir = Path(args.out_dir).resolve()
    targets = [out_dir / "cf.xlsx", out_dir / "icons.xlsx", out_dir / "dv.xlsx"]
    for path in targets:
        if path.exists():
            raise SystemExit(f"refusing to overwrite existing file: {path}")
    out_dir.mkdir(parents=True, exist_ok=True)
    cf(targets[0])
    icons(targets[1])
    dv(targets[2])
    for path in targets:
        print(path)


if __name__ == "__main__":
    main()
