#!/usr/bin/env python3
"""Create the QA-01.1 minimal formula/edit compatibility samples.

The package is deliberately hand-written with the standard library so the
fixtures can be rebuilt on a machine that has no openpyxl installed.  Output
paths are always new files; the fixture names are stable so the manifest and
the Rust regression can refer to them.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
import sys
from datetime import datetime, timezone
from pathlib import Path

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

STYLES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
  <fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>
  <borders count="1"><border/></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs>
</styleSheet>"""


def workbook_xml(sheet_name: str) -> str:
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="{sheet_name}" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""


def workbook_rels() -> str:
    return """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"""


def worksheet(formula: bool) -> str:
    if formula:
        data = """<row r="1"><c r="A1"><v>1</v></c><c r="B1" t="e"><f>1/0</f><v>#DIV/0!</v></c><c r="C1" t="inlineStr"><is><t>财务</t></is></c><c r="D1"><f>A1=1</f><v>1</v></c></row>
<row r="2"><c r="A2"><v>2</v></c><c r="B2" t="str"><f>IFERROR(B1,&quot;ok&quot;)</f><v>ok</v></c><c r="C2" t="str"><f>CONCATENATE(&quot;财&quot;,&quot;务&quot;)</f><v>财务</v></c></row>
<row r="3"><c r="A3"><f>SUM(A1:A2)</f><v>3</v></c><c r="B3"><f>A1+2</f><v>3</v></c></row>
<row r="4"><c r="A4"><f>DATE(1900,1,1)</f><v>1</v></c></row>"""
        dim = "A1:D4"
    else:
        data = """<row r="1"><c r="A1" t="inlineStr"><is><t>old</t></is></c><c r="B1" t="inlineStr"><is><t>second</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>existing</t></is></c></row>"""
        dim = "A1:B2"
    return f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <dimension ref="{dim}"/>
  <sheetData>{data}</sheetData>
</worksheet>"""


def write_xlsx(path: Path, sheet_name: str, formula: bool) -> None:
    if path.exists():
        raise SystemExit(f"refusing to overwrite existing file: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    import zipfile

    with zipfile.ZipFile(path, "x", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("[Content_Types].xml", CONTENT_TYPES)
        archive.writestr("_rels/.rels", ROOT_RELS)
        archive.writestr("xl/workbook.xml", workbook_xml(sheet_name))
        archive.writestr("xl/_rels/workbook.xml.rels", workbook_rels())
        archive.writestr("xl/styles.xml", STYLES)
        archive.writestr("xl/worksheets/sheet1.xml", worksheet(formula))


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", required=True, help="new directory for the synthetic fixtures")
    parser.add_argument("--manifest", required=True, help="new manifest JSON path")
    args = parser.parse_args()

    out_dir = Path(args.out_dir).resolve()
    manifest_path = Path(args.manifest).resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"output directory is not empty: {out_dir}")
    if manifest_path.exists():
        raise SystemExit(f"refusing to overwrite manifest: {manifest_path}")

    formula_path = out_dir / "qa01_formula_min.xlsx"
    edit_path = out_dir / "qa01_edit_min.xlsx"
    write_xlsx(formula_path, "Formula", True)
    write_xlsx(edit_path, "Edit", False)

    manifest = {
        "schema": "qa01-min-formula-edit-v1",
        "generated_utc": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "generator": {
            "script": "scripts/qa01_min_samples.py",
            "python": platform.python_version(),
            "date_system": "1900",
            "calculation_mode": "automatic",
            "locale": "zh-CN",
            "note": "hand-written OOXML; no Office application is used to create the fixtures",
        },
        "fixtures": [
            {
                "id": "formula-min",
                "path": formula_path.name,
                "sha256": sha256(formula_path),
                "function": "formula",
                "expected": {
                    "A1": {"type": "number", "value": 1},
                    "A2": {"type": "number", "value": 2},
                    "A3": {"type": "formula", "formula": "SUM(A1:A2)", "type": "number", "value": 3},
                    "B1": {"type": "formula", "formula": "1/0", "type": "error", "value": "#DIV/0!"},
                    "B2": {"type": "formula", "formula": "IFERROR(B1,\"ok\")", "type": "text", "value": "ok"},
                    "B3": {"type": "formula", "formula": "A1+2", "type": "number", "value": 3},
                    "C1": {"type": "text", "value": "财务"},
                    "C2": {"type": "formula", "formula": "CONCATENATE(\"财\",\"务\")", "type": "text", "value": "财务"},
                    "D1": {"type": "formula", "formula": "A1=1", "type": "boolean", "value": True},
                    "A4": {"type": "formula", "formula": "DATE(1900,1,1)", "type": "number", "value": 1},
                },
            },
            {
                "id": "edit-min",
                "path": edit_path.name,
                "sha256": sha256(edit_path),
                "function": "edit",
                "expected": {
                    "A1": {"type": "text", "value": "old"},
                    "B1": {"type": "text", "value": "second"},
                    "A2": {"type": "text", "value": "existing"},
                },
            },
        ],
        "roundtrip": {
            "project_path": "Excel/WPS value -> sheetcore load/recalc -> save -> reload",
            "tolerance": {"number": "exact for the small integers in this sample", "text": "exact", "error": "exact code"},
            "unimplemented": [],
        },
        "objects": {"charts": 0, "pictures": 0, "textboxes": 0},
        "refreshability": "formula-only sample is recalculated on load; no external data",
    }
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"formula": str(formula_path), "edit": str(edit_path), "manifest": str(manifest_path)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
