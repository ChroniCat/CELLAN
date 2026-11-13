#!/usr/bin/env python3
"""QA-01.2 minimal three-party round-trip runner.

For each path the runner:
  1. asks the source application to open and save the synthetic fixture,
  2. sends that application-produced file through the project core,
  3. reopens the project output in the target application and records the
     actual values/formulas.

Only files under --out-dir are created.  Existing user documents and running
Office instances are never touched: DispatchEx creates a separate automation
instance for each source/target step.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import win32com.client

CELLS = ["A1", "A2", "A3", "B1", "B2", "B3", "C1", "C2", "D1", "A4"]


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def app_name(product: str) -> str:
    return "Excel.Application" if product == "excel" else "ket.Application"


def read_values(book) -> dict:
    result = {}
    sheet = book.Worksheets(1)
    for address in CELLS:
        cell = sheet.Range(address)
        result[address] = {
            "value": cell.Value2,
            "text": cell.Text,
            "formula": cell.Formula,
        }
    return result


def save_as(product: str, src: Path, dst: Path, out_dir: Path) -> tuple[dict, str | None]:
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
        # Reopen the saved copy so the result is read from the same persisted
        # file the next step will receive, not from the pre-save in-memory book.
        book.Close(False)
        book = None
        book = app.Workbooks.Open(str(dst), ReadOnly=True, UpdateLinks=0)
        app.Calculate()
        values = read_values(book)
        info = {
            "product": product,
            "version": str(app.Version),
            "build": str(getattr(app, "Build", "")),
            "file": str(dst),
            "sha256": sha256(dst),
            "values": values,
        }
        return info, None
    except Exception as error:
        return {"product": product, "error": f"{type(error).__name__}: {error}"}, f"{product} source save/open failed: {error}"
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


def read_with(product: str, path: Path) -> tuple[dict, str | None]:
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
        app.Calculate()
        values = read_values(book)
        return {
            "product": product,
            "version": str(app.Version),
            "build": str(getattr(app, "Build", "")),
            "file": str(path),
            "sha256": sha256(path),
            "values": values,
        }, None
    except Exception as error:
        return {"product": product, "error": f"{type(error).__name__}: {error}"}, f"{product} target open failed: {error}"
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


def expected_from_manifest(manifest: dict) -> dict:
    fixture = next(f for f in manifest["fixtures"] if f["id"] == "formula-min")
    return fixture["expected"]


def normalize(value):
    if isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def compare(actual: dict, expected: dict) -> list[str]:
    differences = []
    for address, want in expected.items():
        got = actual.get(address, {})
        if want["type"] == "error":
            text = str(got.get("text", ""))
            if text != want["value"]:
                differences.append(f"{address}: expected {want['value']} error text, got {text!r}")
        elif want["type"] == "text":
            text = str(got.get("value", ""))
            if text != want["value"]:
                differences.append(f"{address}: expected text {want['value']!r}, got {text!r}")
        else:
            value = normalize(got.get("value"))
            if value != want["value"]:
                differences.append(f"{address}: expected {want['value']!r}, got {value!r}")
        if "formula" in want:
            formula = str(got.get("formula", ""))
            if formula != "=" + want["formula"]:
                differences.append(f"{address}: expected formula ={want['formula']}, got {formula!r}")
    return differences


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
        return result, f"core failed with exit {proc.returncode}: {proc.stderr.strip()}"
    return result, None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--core-exe", default="target/debug/examples/qa01_roundtrip.exe")
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    fixture = Path(args.fixture).resolve()
    manifest_path = Path(args.manifest).resolve()
    core_exe = Path(args.core_exe).resolve()
    out_dir = Path(args.out_dir).resolve()
    out_json = Path(args.out).resolve()
    if not fixture.exists():
        raise SystemExit(f"fixture missing: {fixture}")
    if not manifest_path.exists():
        raise SystemExit(f"manifest missing: {manifest_path}")
    if not core_exe.exists():
        raise SystemExit(f"core helper missing (build it first): {core_exe}")
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"out-dir is not empty: {out_dir}")
    if out_json.exists():
        raise SystemExit(f"refusing to overwrite output JSON: {out_json}")
    out_dir.mkdir(parents=True, exist_ok=True)
    out_json.parent.mkdir(parents=True, exist_ok=True)

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    expected = expected_from_manifest(manifest)
    paths = [
        ("excel-project-excel", "excel", "excel"),
        ("wps-project-wps", "wps", "wps"),
        ("excel-project-wps", "excel", "wps"),
    ]
    results = []
    failed = False
    for idx, (label, source, target) in enumerate(paths, 1):
        case_dir = out_dir / f"{idx:02d}-{label}"
        case_dir.mkdir(parents=True, exist_ok=False)
        source_file = case_dir / f"{source}-input.xlsx"
        core_file = case_dir / "project-output.xlsx"
        source_info, source_error = save_as(source, fixture, source_file, case_dir)
        core_info = None
        target_info = None
        target_error = None
        differences = []
        if source_error is None:
            core_info, core_error = run_core(core_exe, source_file, core_file)
            if core_error is not None:
                differences.append(core_error)
            else:
                target_info, target_error = read_with(target, core_file)
                if target_error is not None:
                    differences.append(target_error)
                else:
                    differences = compare(target_info["values"], expected)
        entry = {
            "id": label,
            "source": source_info,
            "target": target_info,
            "core": core_info,
            "expected": expected,
            "differences": differences,
            "status": "pass" if not differences else "fail",
        }
        if source_error is not None:
            entry["source_error"] = source_error
            entry["status"] = "fail"
        results.append(entry)
        if entry["status"] != "pass":
            failed = True

    document = {
        "schema": "qa01-three-party-roundtrip-v1",
        "generated_utc": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "fixture": str(fixture),
        "fixture_sha256": sha256(fixture),
        "manifest": str(manifest_path),
        "core_exe": str(core_exe),
        "paths": results,
        "status": "pass" if not failed else "fail",
    }
    out_json.write_text(json.dumps(document, ensure_ascii=False, indent=2, default=str) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(document, ensure_ascii=False, indent=2, default=str))
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
