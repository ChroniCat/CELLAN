#!/usr/bin/env python3
"""QA-01.3 chart/page three-party smoke runner.

This is deliberately narrower than the full QA-01 matrix: it checks the
project's synthetic chart and print fixtures through the same three paths as
the formula runner.  Failure or an unexpected repair/error is recorded, never
turned into a pass.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import win32com.client

from qa01_roundtrip import app_name, sha256


def inspect(product: str, path: Path, kind: str) -> tuple[dict | None, str | None]:
    app = None
    book = None
    try:
        app = win32com.client.DispatchEx(app_name(product))
        app.Visible = False
        try:
            app.DisplayAlerts = False
        except Exception:
            pass
        book = app.Workbooks.Open(str(path), ReadOnly=True, UpdateLinks=0)
        sheet = book.Worksheets(1)
        charts = 0
        pivots: list[dict] = []
        for si in range(1, int(book.Worksheets.Count) + 1):
            sh = book.Worksheets(si)
            charts += int(sh.ChartObjects().Count)
            for pi in range(1, int(sh.PivotTables().Count) + 1):
                pt = sh.PivotTables(pi)
                fields = []
                for fi in range(1, int(pt.PivotFields().Count) + 1):
                    f = pt.PivotFields(fi)
                    fields.append({"name": str(f.Name), "orientation": int(f.Orientation)})
                pivots.append({"sheet": str(sh.Name), "name": str(pt.Name), "fields": fields})
        info: dict = {
            "product": product,
            "version": str(app.Version),
            "build": str(getattr(app, "Build", "")),
            "file": str(path),
            "sha256": sha256(path),
            "chart_objects": charts,
            "pivot_tables": pivots,
            "page": {
                "orientation": int(sheet.PageSetup.Orientation),
                "paper": int(sheet.PageSetup.PaperSize),
                "zoom": int(sheet.PageSetup.Zoom),
                "print_area": str(sheet.PageSetup.PrintArea),
                "title_rows": str(sheet.PageSetup.PrintTitleRows),
                "left_margin": float(sheet.PageSetup.LeftMargin),
                # 打印网格线，不是窗口屏幕网格线（ActiveWindow.DisplayGridlines）
                "print_gridlines": bool(sheet.PageSetup.PrintGridlines),
            },
        }
        if kind == "chart":
            info["chart_titles"] = [
                str(book.Worksheets(si).ChartObjects(ci + 1).Chart.ChartTitle.Text)
                for si in range(1, int(book.Worksheets.Count) + 1)
                for ci in range(int(book.Worksheets(si).ChartObjects().Count))
            ]
        return info, None
    except Exception as error:
        return None, f"{product} open/inspect failed for {path.name}: {type(error).__name__}: {error}"
    finally:
        if book is not None:
            try:
                book.Close(False)
            except Exception:
                pass
        if app is not None:
            try:
                app.Quit()
            except Exception:
                pass


def save_source(product: str, src: Path, dst: Path) -> tuple[dict | None, str | None]:
    app = None
    book = None
    try:
        app = win32com.client.DispatchEx(app_name(product))
        app.Visible = False
        try:
            app.DisplayAlerts = False
        except Exception:
            pass
        book = app.Workbooks.Open(str(src), ReadOnly=False, UpdateLinks=0)
        book.SaveAs(str(dst), FileFormat=51)
        return inspect(product, dst, "chart" if "chart" in src.name else "page")
    except Exception as error:
        return None, f"{product} source save failed for {src.name}: {type(error).__name__}: {error}"
    finally:
        if book is not None:
            try:
                book.Close(False)
            except Exception:
                pass
        if app is not None:
            try:
                app.Quit()
            except Exception:
                pass


def run_core(core_exe: Path, src: Path, dst: Path) -> tuple[dict, str | None]:
    proc = subprocess.run(
        [str(core_exe), str(src), str(dst)],
        cwd=str(Path(__file__).resolve().parent.parent),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=120,
    )
    result = {
        "command": [str(core_exe), str(src), str(dst)],
        "exit": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
        "output_exists": dst.exists(),
        "output_sha256": sha256(dst) if dst.exists() else None,
    }
    if proc.returncode != 0 or not dst.exists():
        return result, f"core failed for {src.name}: exit {proc.returncode}: {proc.stderr.strip()}"
    return result, None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", required=True)
    parser.add_argument("--core-exe", default="target/debug/examples/qa01_roundtrip.exe")
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--kinds", default="chart,page,pivot", help="comma-separated: chart,page,pivot")
    args = parser.parse_args()
    kinds = [k.strip() for k in args.kinds.split(",") if k.strip()]
    if not kinds or any(k not in ("chart", "page", "pivot") for k in kinds):
        raise SystemExit(f"invalid --kinds: {args.kinds}")
    fixtures = Path(args.fixtures).resolve()
    core_exe = Path(args.core_exe).resolve()
    out_dir = Path(args.out_dir).resolve()
    out_json = Path(args.out).resolve()
    if not fixtures.is_dir():
        raise SystemExit(f"fixtures directory missing: {fixtures}")
    if not core_exe.exists():
        raise SystemExit(f"core helper missing: {core_exe}")
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"out-dir is not empty: {out_dir}")
    if out_json.exists():
        raise SystemExit(f"refusing to overwrite output: {out_json}")
    out_dir.mkdir(parents=True, exist_ok=True)
    out_json.parent.mkdir(parents=True, exist_ok=True)

    cases = []
    for kind in kinds:
        src_fixture = fixtures / f"{kind}.xlsx"
        if not src_fixture.exists():
            raise SystemExit(f"fixture missing: {src_fixture}")
        for source, target in [("excel", "excel"), ("wps", "wps"), ("excel", "wps")]:
            label = f"{kind}-{source}-project-{target}"
            case_dir = out_dir / label
            case_dir.mkdir(parents=True, exist_ok=False)
            source_file = case_dir / f"{source}-input.xlsx"
            output_file = case_dir / "project-output.xlsx"
            source_info, source_error = save_source(source, src_fixture, source_file)
            core_info = None
            target_info = None
            differences = []
            if source_error is None:
                core_info, core_error = run_core(core_exe, source_file, output_file)
                if core_error:
                    differences.append(core_error)
                else:
                    target_info, target_error = inspect(target, output_file, kind)
                    if target_error:
                        differences.append(target_error)
                    else:
                        if kind == "chart":
                            if int(source_info["chart_objects"]) != 1:
                                differences.append(f"source chart count is {source_info['chart_objects']}, expected 1")
                            if int(target_info["chart_objects"]) != 1:
                                differences.append(f"target chart count is {target_info['chart_objects']}, expected 1")
                            if source_info["chart_titles"] != target_info["chart_titles"]:
                                differences.append(f"chart title changed: {source_info['chart_titles']} -> {target_info['chart_titles']}")
                        elif kind == "pivot":
                            if not source_info["pivot_tables"]:
                                differences.append("source has no pivot table")
                            if len(source_info["pivot_tables"]) != len(target_info["pivot_tables"]):
                                differences.append(
                                    f"pivot count: {len(source_info['pivot_tables'])} -> {len(target_info['pivot_tables'])}"
                                )
                            elif source_info["pivot_tables"] != target_info["pivot_tables"]:
                                differences.append(
                                    f"pivot changed: {source_info['pivot_tables']} -> {target_info['pivot_tables']}"
                                )
                        else:
                            for key, want in {
                                "orientation": 2,  # xlLandscape
                                "paper": 8,
                                "zoom": 85,
                                "print_area": "$A$1:$H$40",
                                "title_rows": "$1:$2",
                                "print_gridlines": True,
                            }.items():
                                got = target_info["page"][key]
                                if str(got) != str(want):
                                    differences.append(f"page {key}: expected {want!r}, got {got!r}")
                            if abs(target_info["page"]["left_margin"] - 36.0) > 0.01:  # COM returns points; 0.5in = 36pt
                                differences.append(f"left margin: {target_info['page']['left_margin']}")
            entry = {
                "id": label,
                "source": source_info,
                "target": target_info,
                "core": core_info,
                "differences": differences,
                "status": "pass" if not differences else "fail",
            }
            if source_error:
                entry["source_error"] = source_error
                entry["status"] = "fail"
            cases.append(entry)

    failed = any(c["status"] != "pass" for c in cases)
    document = {
        "schema": "qa01-extended-roundtrip-v1",
        "generated_utc": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "fixtures": str(fixtures),
        "fixtures_sha256": {
            name: sha256(fixtures / name)
            for name in ["chart.xlsx", "page.xlsx", "pivot.xlsx"]
            if (fixtures / name).exists()
        },
        "core_exe": str(core_exe),
        "cases": cases,
        "status": "pass" if not failed else "fail",
    }
    out_json.write_text(json.dumps(document, ensure_ascii=False, indent=2, default=str) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(document, ensure_ascii=False, indent=2, default=str))
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
