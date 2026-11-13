#!/usr/bin/env python3
"""生成 app UI 回归用的合成夹具（QA-02.1）。

输出目录必须为空或不存在；不写用户目录，不覆盖已有夹具。
"""
import shutil
import sys
from pathlib import Path

from openpyxl import Workbook
from openpyxl.drawing.image import Image
from openpyxl.drawing.spreadsheet_drawing import AnchorMarker, TwoCellAnchor
from PIL import Image as PILImage

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "crates/app/tests/fixtures/qa02_app"
    if out.exists() and any(out.iterdir()):
        print(f"refusing to overwrite non-empty output: {out}", file=sys.stderr)
        return 2
    out.mkdir(parents=True, exist_ok=True)

    shutil.copyfile(ROOT / "crates/core/tests/fixtures/qa02/table.xlsx", out / "table.xlsx")
    shutil.copyfile(ROOT / "crates/core/tests/fixtures/qa02_cf_dv/icons.xlsx", out / "icons.xlsx")

    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "产品表"
    for row, values in enumerate(
        [("产品", "数量", "单价"), ("A", 1, 10), ("B", 2, 20), ("C", 3, 30)], 1
    ):
        for col, value in enumerate(values, 1):
            ws.cell(row=row, column=col, value=value)
    wb.save(out / "compare_left.xlsx")

    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws.title = "一月"
    for col, value in enumerate([1, 2, 3, 4, 5], 1):
        ws.cell(row=1, column=col, value=value)
    ws2 = wb.create_sheet("二月")
    for col, value in enumerate([5, 4, 3, 2, 1], 1):
        ws2.cell(row=1, column=col, value=value)
    wb.save(out / "spark_table.xlsx")

    # 图片：C3:D8 的 twoCellAnchor，与 app UI 回归的 at=(2,1,7,3) 契约一致。
    pixel = out / "pixel.png"
    PILImage.new("RGB", (16, 16), (30, 120, 200)).save(pixel)
    wb = Workbook()
    ws = wb.active
    assert ws is not None
    ws["A1"] = "图片夹具"
    image = Image(str(pixel))
    image.anchor = TwoCellAnchor(
        _from=AnchorMarker(col=1, colOff=0, row=2, rowOff=0),
        to=AnchorMarker(col=3, colOff=0, row=7, rowOff=0),
        editAs="oneCell",
    )
    ws.add_image(image)
    wb.save(out / "pic.xlsx")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
