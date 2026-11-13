#!/usr/bin/env python3
"""Read the QA-01.1 synthetic workbook in installed Excel and WPS.

This is a small, read-only oracle.  It opens only the generated fixture passed
on the command line, closes that workbook, and never touches a user document or
a WPS/Excel instance that was already running.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import win32com.client


def read_product(product: str, progid: str, fixture: Path) -> dict:
    app = None
    book = None
    try:
        app = win32com.client.DispatchEx(progid)
        app.Visible = False
        book = app.Workbooks.Open(str(fixture), ReadOnly=True, UpdateLinks=0)
        app.Calculate()
        cells = {}
        for address in ["A1", "A2", "A3", "B1", "B2", "B3", "C1", "C2", "D1", "A4"]:
            cell = book.Worksheets(1).Range(address)
            cells[address] = {
                "value": cell.Value2,
                "text": cell.Text,
                "formula": cell.Formula,
            }
        return {
            "product": product,
            "version": str(app.Version),
            "build": str(getattr(app, "Build", "")),
            "cells": cells,
        }
    except Exception as error:  # report the actual failure, do not pretend it opened
        return {"product": product, "error": f"{type(error).__name__}: {error}"}
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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    fixture = Path(args.fixture).resolve()
    out = Path(args.out).resolve()
    if not fixture.exists():
        raise SystemExit(f"fixture missing: {fixture}")
    if out.exists():
        raise SystemExit(f"refusing to overwrite existing output: {out}")
    result = {
        "schema": "qa01-min-oracle-v1",
        "fixture": str(fixture),
        "products": [
            read_product("excel", "Excel.Application", fixture),
            read_product("wps", "ket.Application", fixture),
        ],
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, ensure_ascii=False, indent=2, default=str) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(result, ensure_ascii=False, default=str))


if __name__ == "__main__":
    main()
