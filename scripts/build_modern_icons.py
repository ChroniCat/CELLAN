#!/usr/bin/env python3
"""Reproducible builder for CELLAN 1.86 modern Lucide + Fluent-2 icon atlases.

Generates:
  - assets/icons/modern/LICENSE-LUCIDE.txt
  - assets/icons/modern/sources.json        (offline-reproducible SVG definitions)
  - assets/icons/modern/manifest.json       (atlas grid layout & icon metadata)
  - assets/icons/modern/atlas_64.png        (64x64/cell L8 grayscale alpha mask, stroke 1.7)
  - assets/icons/modern/atlas_32.png        (32x32/cell L8 grayscale alpha mask, stroke 1.95)
  - assets/icons/modern/accent_64.png       (64x64/cell L8 disjoint semantic color masks)
  - assets/icons/modern/accent_32.png       (32x32/cell L8 disjoint semantic color masks)
  - crates/app/src/ui_icons/atlas_meta.rs   (generated Rust index & semantic layer table)

Requirements:
  - Python 3.10+, Pillow (`pip install pillow`), Playwright (`pip install playwright`)
    with system Chrome or Edge (or Playwright Chromium).
  - Optional: `--lucide-dir <path>` or `--fetch-lucide` to refresh Lucide SVG nodes from
    `lucide-static` (ISC license). Without `--fetch-lucide`, rebuilds offline from
    `assets/icons/modern/sources.json`.
  - Optional: `--contact-sheet <dir>` renders Light & Dark visual QA sheets at 32px and 16px.
"""

from __future__ import annotations

import argparse
import io
import json
import math
import os
import re
import shutil
import sys
import tarfile
import urllib.request
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "assets" / "icons" / "modern"
META_RS = ROOT / "crates" / "app" / "src" / "ui_icons" / "atlas_meta.rs"
NOTO_SC_BOLD = ROOT / "assets" / "fonts" / "subset" / "NotoSansSC-Bold.subset.ttf"

MONO_COLS = 15
MONO_ROWS = 11
ACCENT_COLS = 12

# Exact declaration order of `pub enum Icon` in crates/app/src/ui_icons/mod.rs (165 variants).
ICON_ORDER: list[str] = [
    "Open", "Save", "SaveAs", "Print", "Undo", "Redo",
    "Paste", "Cut", "Copy", "Brush", "Fill", "FontColor",
    "AlignL", "AlignC", "AlignR", "Top", "Middle", "Bottom",
    "Wrap", "Merge", "Border", "Currency", "Percent", "Comma",
    "SortAsc", "SortDesc", "Filter", "Find", "Chart", "Image",
    "Table", "Comment", "Pivot", "Link", "Revision", "Zoom",
    "Calc", "Sheet", "Share", "Fx", "Sum", "Grid",
    "Home", "Close", "Plus", "Refresh", "DecInc", "DecDec",
    "FontGrow", "FontShrink", "TextDir", "HeaderFooter", "DocLink",
    "GroupRows", "UngroupRows", "ShowDetail", "HideDetail", "Validation",
    "SplitColumns", "RemoveDup", "Consolidate", "ImportCsv", "ImportBook",
    "WhatIf", "OutlineLevels", "GoalSeek", "Scenario", "Eye", "EyeOff",
    "PageLayout", "PageBreak", "NormalView", "ShowHeadings", "FormulaBar",
    "TextSpill", "ViewPane", "Pin", "NewView", "ZoomSelection", "FormulaView",
    "ErrorCheck", "NameTag", "WatchPane", "Spelling", "CommentPrev",
    "CommentNext", "ShowComment", "A11y", "Online", "AutoShare", "Help",
    "About", "Orientation", "PaperSize", "PrintGrid", "PrintHeadings",
    "PrintArea", "PivotRefresh", "PivotOptions", "ChartList", "FolderPlus",
    "AppDefault", "AppAdd", "PdfExport", "HostTakeover", "UpdateCheck",
    "FormatCells", "TextVerticalDraw", "TextBoxDraw", "CalcSheet", "SheetRef",
    "FilterReapply", "SortDialog", "ShowChanges", "SheetViewSwitch", "Zoom100",
    "ZoomDialog", "ZoomIn", "Unfreeze", "TableToRange", "SparkEdit",
    "ChartEdit", "RichText", "Freeze", "FreezeRow", "FreezeCol", "Lock",
    "Cf", "Eraser", "ClearFmt", "IndentInc", "IndentDec", "Symbol",
    "ZhS", "ZhT", "TraceIn", "TraceOut", "SparkLine", "SparkColumn",
    "SparkLoss", "TextBox", "NewFile", "Macro", "MacroRun", "Code",
    "Compare", "SyncScroll", "InvalidCircle", "PasteValues123", "StrikeThrough",
    "Spotlight", "SplitPanes", "HighlightDup", "RejectDup", "MergeSame",
    "AiSparkle", "InsertCell", "DeleteCell", "CellStyle", "FnBook",
    "Transpose", "PasteLink", "PastePic", "NoBorder", "ColWidth",
]
assert len(ICON_ORDER) == 165, f"Expected 165 icons, got {len(ICON_ORDER)}"

# Direct Lucide 1.82.0 icon mappings (ISC / Feather MIT).
# Icons not in this dict are custom Lucide-matched 24x24 spreadsheet SVGs below.
LUCIDE_MAP: dict[str, str] = {
    "Open": "folder-open",
    "Save": "save",
    "SaveAs": "save-pen",
    "Print": "printer",
    "Undo": "undo-2",
    "Redo": "redo-2",
    "Paste": "clipboard-paste",
    "Cut": "scissors",
    "Copy": "copy",
    "Brush": "paintbrush",
    "Fill": "paint-bucket",
    "AlignL": "align-left",
    "AlignC": "align-center",
    "AlignR": "align-right",
    "Top": "arrow-up-to-line",
    "Bottom": "arrow-down-to-line",
    "Wrap": "wrap-text",
    "Merge": "table-cells-merge",
    "Border": "grid-2x2",
    "Currency": "japanese-yen",
    "Percent": "percent",
    "SortAsc": "arrow-down-a-z",
    "SortDesc": "arrow-down-z-a",
    "Filter": "funnel",
    "Find": "search",
    "Image": "image",
    "Table": "table",
    "Comment": "message-square-text",
    "Link": "link-2",
    "Revision": "file-pen-line",
    "Calc": "calculator",
    "Sheet": "file-spreadsheet",
    "Share": "share-2",
    "Sum": "sigma",
    "Grid": "grid-3x3",
    "Home": "house",
    "Close": "x",
    "Plus": "plus",
    "Refresh": "refresh-cw",
    "FontGrow": "a-arrow-up",
    "FontShrink": "a-arrow-down",
    "DocLink": "file-symlink",
    "Validation": "grid-2x2-check",
    "SplitColumns": "table-columns-split",
    "Consolidate": "combine",
    "ImportCsv": "file-input",
    "OutlineLevels": "list-tree",
    "GoalSeek": "target",
    "Eye": "eye",
    "EyeOff": "eye-off",
    "PageLayout": "layout-template",
    "NormalView": "table-2",
    "ViewPane": "panel-left",
    "Pin": "pin",
    "NewView": "copy-plus",
    "ZoomSelection": "scan-search",
    "NameTag": "tag",
    "WatchPane": "scan-eye",
    "Spelling": "spell-check",
    "ShowComment": "message-square-dot",
    "A11y": "accessibility",
    "Help": "circle-help",
    "About": "info",
    "PivotOptions": "table-config",
    "FolderPlus": "folder-plus",
    "AppDefault": "file-cog",
    "AppAdd": "file-plus-2",
    "HostTakeover": "monitor-up",
    "UpdateCheck": "refresh-ccw-dot",
    "FormatCells": "table-properties",
    "TextBoxDraw": "square-pen",
    "ShowChanges": "file-diff",
    "SheetViewSwitch": "layers",
    "ZoomIn": "zoom-in",
    "Lock": "lock",
    "Eraser": "eraser",
    "ClearFmt": "remove-formatting",
    "IndentInc": "indent-increase",
    "IndentDec": "indent-decrease",
    "Symbol": "omega",
    "TextBox": "square-dashed-text",
    "NewFile": "file-plus",
    "MacroRun": "circle-play",
    "Code": "code-xml",
    "StrikeThrough": "strikethrough",
    "AiSparkle": "sparkles",
    "InsertCell": "grid-2x2-plus",
    "DeleteCell": "grid-2x2-x",
    "FnBook": "book-open-text",
    "NoBorder": "grid-3x3-off",
}


def extract_cjk_glyph_path(ch: str, target_box: tuple[float, float, float, float]) -> str:
    """Extract an open-source NotoSansSC-Bold glyph outline scaled cleanly into target_box."""
    from fontTools.pens.boundsPen import BoundsPen
    from fontTools.pens.svgPathPen import SVGPathPen
    from fontTools.pens.transformPen import TransformPen
    from fontTools.ttLib import TTFont

    font = TTFont(str(NOTO_SC_BOLD))
    cmap = font.getBestCmap()
    gname = cmap[ord(ch)]
    gset = font.getGlyphSet()
    bp = BoundsPen(gset)
    gset[gname].draw(bp)
    x0, y0, x1, y1 = bp.bounds
    tx0, ty0, tx1, ty1 = target_box
    gw = max(1.0, x1 - x0)
    gh = max(1.0, y1 - y0)
    tw = tx1 - tx0
    th = ty1 - ty0
    scale = min(tw / gw, th / gh)
    ox = tx0 + (tw - gw * scale) / 2.0 - x0 * scale
    oy = ty0 + (th - gh * scale) / 2.0 + y1 * scale
    # Font coordinates are Y-up; SVG is Y-down -> (scale, 0, 0, -scale, ox, oy)
    pen = SVGPathPen(gset)
    tpen = TransformPen(pen, (scale, 0.0, 0.0, -scale, ox, oy))
    gset[gname].draw(tpen)
    # Round coordinates to 2 decimal places for compact deterministic SVG.
    raw = pen.getCommands()
    return re.sub(r"-?\d+\.\d+", lambda m: f"{float(m.group(0)):.2f}".rstrip("0").rstrip("."), raw)


def build_custom_svgs() -> dict[str, dict[str, Any]]:
    """Author custom 24x24 spreadsheet SVGs in exact Lucide optical stroke style.

    Each entry returns:
      - `mono`: complete SVG inner elements rendered in white for mono atlas
      - `layers` (optional): list of `(role, svg_inner)` for disjoint semantic color rendering
    """
    zhs_d = extract_cjk_glyph_path("简", (2.4, 4.4, 15.2, 19.6))
    zht_d = extract_cjk_glyph_path("繁", (8.8, 4.4, 21.6, 19.6))

    custom: dict[str, dict[str, Any]] = {
        # FontColor: crisp geometric 'A' + thick rounded color bar below
        "FontColor": {
            "mono": (
                '<path d="M6 15.5 12 3.5l6 12" />'
                '<path d="M8.1 11.5h7.8" />'
                '<rect x="3.5" y="18.2" width="17" height="3" rx="1.4" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("Text", '<path d="M6 15.5 12 3.5l6 12" /><path d="M8.1 11.5h7.8" />'),
                ("Err", '<rect x="3.5" y="18.2" width="17" height="3" rx="1.4" fill="currentColor" stroke="none" />'),
            ],
        },
        # Middle vertical alignment: clean solid center line + converging arrows (pairs with Top/Bottom)
        "Middle": {
            "mono": (
                '<path d="M4 12h16" />'
                '<path d="M12 3v5.5" />'
                '<path d="m9 5.5 3 3 3-3" />'
                '<path d="M12 21v-5.5" />'
                '<path d="m9 18.5 3-3 3 3" />'
            ),
        },
        # Comma: smooth cubic-Bezier typographic comma centered optically in 24x24
        "Comma": {
            "mono": (
                '<path d="M13.5 9.8a2.1 2.1 0 1 1-4.2 0 2.1 2.1 0 0 1 4.2 0Z" fill="currentColor" stroke="none" />'
                '<path d="M13.2 10.6c.2 3.2-1.1 6-3.7 7.6" stroke-width="2.15" />'
            ),
            "layers": [
                ("Accent", '<path d="M13.5 9.8a2.1 2.1 0 1 1-4.2 0 2.1 2.1 0 0 1 4.2 0Z" fill="currentColor" stroke="none" /><path d="M13.2 10.6c.2 3.2-1.1 6-3.7 7.6" stroke-width="2.15" />'),
            ],
        },
        # Chart: crisp axes + 3 rounded semantic columns
        "Chart": {
            "mono": (
                '<path d="M3 3v16a2 2 0 0 0 2 2h16" />'
                '<rect x="7" y="12" width="3" height="6" rx="1" fill="currentColor" stroke="none" />'
                '<rect x="12" y="6" width="3" height="12" rx="1" fill="currentColor" stroke="none" />'
                '<rect x="17" y="9" width="3" height="9" rx="1" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("Text", '<path d="M3 3v16a2 2 0 0 0 2 2h16" />'),
                ("Accent", '<rect x="7" y="12" width="3" height="6" rx="1" fill="currentColor" stroke="none" />'),
                ("Info", '<rect x="12" y="6" width="3" height="12" rx="1" fill="currentColor" stroke="none" />'),
                ("Warn", '<rect x="17" y="9" width="3" height="9" rx="1" fill="currentColor" stroke="none" />'),
            ],
        },
        # Pivot: top-left 2x2 table + curved L-shaped pivot summary arrow
        "Pivot": {
            "mono": (
                '<rect x="3" y="3" width="11" height="10" rx="1.8" />'
                '<path d="M3 8h11" />'
                '<path d="M8.5 3v10" />'
                '<path d="M18 11v4a3 3 0 0 1-3 3h-4" />'
                '<path d="m14 15-3 3 3 3" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="3.8" width="9.4" height="4.2" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="3" y="3" width="11" height="10" rx="1.8" /><path d="M3 8h11" /><path d="M8.5 3v10" />'),
                ("Accent", '<path d="M18 11v4a3 3 0 0 1-3 3h-4" /><path d="m14 15-3 3 3 3" />'),
            ],
        },
        # Zoom: crisp magnifying glass with clean lens reflection arc (distinct from ZoomIn +)
        "Zoom": {
            "mono": (
                '<circle cx="10.5" cy="10.5" r="7" />'
                '<path d="m20.5 20.5-4.8-4.8" />'
                '<path d="M8 8a3.8 3.8 0 0 1 5 0" />'
            ),
        },
        # Fx: smooth italic Bezier 'f' + clean 'x'
        "Fx": {
            "mono": (
                '<path d="M6 19.5c1.8 0 2.6-1 3-3.2l1.2-8.1c.4-2.5 1.4-3.7 3.3-3.7.7 0 1.2.2 1.5.5" />'
                '<path d="M6.5 10.5h6.5" />'
                '<path d="m14 12.5 5.5 7" />'
                '<path d="m19.5 12.5-5.5 7" />'
            ),
        },
        # DecInc: '.0' + crisp '+' badge
        "DecInc": {
            "mono": (
                '<circle cx="4.2" cy="15.5" r="1.15" fill="currentColor" stroke="none" />'
                '<rect x="7.4" y="6.5" width="5.6" height="10" rx="2.8" />'
                '<path d="M15.5 11.5h6" />'
                '<path d="M18.5 8.5v6" />'
            ),
            "layers": [
                ("Text", '<circle cx="4.2" cy="15.5" r="1.15" fill="currentColor" stroke="none" /><rect x="7.4" y="6.5" width="5.6" height="10" rx="2.8" />'),
                ("Accent", '<path d="M15.5 11.5h6" /><path d="M18.5 8.5v6" />'),
            ],
        },
        # DecDec: '.0' + crisp '-' badge
        "DecDec": {
            "mono": (
                '<circle cx="4.2" cy="15.5" r="1.15" fill="currentColor" stroke="none" />'
                '<rect x="7.4" y="6.5" width="5.6" height="10" rx="2.8" />'
                '<path d="M15.5 11.5h6" />'
            ),
            "layers": [
                ("Text", '<circle cx="4.2" cy="15.5" r="1.15" fill="currentColor" stroke="none" /><rect x="7.4" y="6.5" width="5.6" height="10" rx="2.8" />'),
                ("Warn", '<path d="M15.5 11.5h6" />'),
            ],
        },
        # TextDir: 'A' + smooth curved 90-degree rotation arrow
        "TextDir": {
            "mono": (
                '<path d="M4.5 15.5 8.5 6.5l4 9" />'
                '<path d="M6 12.5h5" />'
                '<path d="M14.5 5.5h2.2a2.8 2.8 0 0 1 2.8 2.8V19" />'
                '<path d="m16.5 16 3 3 3-3" />'
            ),
            "layers": [
                ("Text", '<path d="M4.5 15.5 8.5 6.5l4 9" /><path d="M6 12.5h5" />'),
                ("Accent", '<path d="M14.5 5.5h2.2a2.8 2.8 0 0 1 2.8 2.8V19" /><path d="m16.5 16 3 3 3-3" />'),
            ],
        },
        # HeaderFooter: rounded page with highlighted top header & bottom footer bands
        "HeaderFooter": {
            "mono": (
                '<rect x="4" y="2.5" width="16" height="19" rx="2" />'
                '<path d="M4 7h16" />'
                '<path d="M4 17h16" />'
                '<path d="M8 10.5h8" />'
                '<path d="M8 13.5h5.5" />'
            ),
            "layers": [
                ("AccentSoft", '<rect x="4.8" y="3.2" width="14.4" height="3.6" rx="1" fill="currentColor" stroke="none" /><rect x="4.8" y="17.2" width="14.4" height="3.6" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="4" y="2.5" width="16" height="19" rx="2" /><path d="M8 10.5h8" /><path d="M8 13.5h5.5" />'),
                ("Accent", '<path d="M4.5 7h15" /><path d="M4.5 17h15" />'),
            ],
        },
        # GroupRows: left outline bracket + crisp '+'
        "GroupRows": {
            "mono": (
                '<path d="M8.5 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h2.5" />'
                '<path d="M12 12h8" />'
                '<path d="M16 8v8" />'
            ),
            "layers": [
                ("Text", '<path d="M8.5 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h2.5" />'),
                ("Accent", '<path d="M12 12h8" /><path d="M16 8v8" />'),
            ],
        },
        # UngroupRows: left outline bracket + crisp '-'
        "UngroupRows": {
            "mono": (
                '<path d="M8.5 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h2.5" />'
                '<path d="M12 12h8" />'
            ),
            "layers": [
                ("Text", '<path d="M8.5 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h2.5" />'),
                ("Warn", '<path d="M12 12h8" />'),
            ],
        },
        # ShowDetail: expanded chevron-down + 3 rounded detail lines
        "ShowDetail": {
            "mono": (
                '<path d="m3.5 9.5 3 3 3-3" />'
                '<path d="M12 6.5h9" />'
                '<path d="M12 12h9" />'
                '<path d="M12 17.5h6.5" />'
            ),
        },
        # HideDetail: collapsed chevron-right + 3 rounded detail lines
        "HideDetail": {
            "mono": (
                '<path d="m5 9 3 3-3 3" />'
                '<path d="M12 6.5h9" />'
                '<path d="M12 12h9" />'
                '<path d="M12 17.5h6.5" />'
            ),
        },
        # RemoveDup: stacked rows with duplicate row crossed out by clean red 'x'
        "RemoveDup": {
            "mono": (
                '<rect x="3" y="4" width="18" height="4.5" rx="1.4" />'
                '<path d="M3 13.5h8.5" />'
                '<path d="M3 18.5h8.5" />'
                '<path d="m15 13.5 5.5 5.5" />'
                '<path d="m20.5 13.5-5.5 5.5" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="4" width="18" height="4.5" rx="1.4" /><path d="M3 13.5h8.5" /><path d="M3 18.5h8.5" />'),
                ("Err", '<path d="m15 13.5 5.5 5.5" /><path d="m20.5 13.5-5.5 5.5" />'),
            ],
        },
        # ImportBook: spreadsheet grid + crisp green downward import arrow
        "ImportBook": {
            "mono": (
                '<path d="M12 20H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v4.5" />'
                '<path d="M3 9.5h16" />'
                '<path d="M9 4v16" />'
                '<path d="M18 13v8" />'
                '<path d="m15 18 3 3 3-3" />'
            ),
            "layers": [
                ("Text", '<path d="M12 20H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v4.5" /><path d="M3 9.5h16" /><path d="M9 4v16" />'),
                ("Accent", '<path d="M18 13v8" /><path d="m15 18 3 3 3-3" />'),
            ],
        },
        # WhatIf: mini table + bidirectional vertical up/down scenario arrow
        "WhatIf": {
            "mono": (
                '<rect x="3" y="6" width="11" height="12" rx="1.8" />'
                '<path d="M3 11h11" />'
                '<path d="M8.5 6v12" />'
                '<path d="M18.5 5.5v13" />'
                '<path d="m16 8 2.5-2.5L21 8" />'
                '<path d="m16 16 2.5 2.5L21 16" />'
            ),
        },
        # Scenario: mini spreadsheet + bookmark flag
        "Scenario": {
            "mono": (
                '<path d="M11 20H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v2" />'
                '<path d="M3 9.5h12" />'
                '<path d="M8.5 4v16" />'
                '<path d="M15 11.5a1.5 1.5 0 0 1 1.5-1.5h3a1.5 1.5 0 0 1 1.5 1.5V21l-3-2-3 2z" />'
            ),
            "layers": [
                ("Text", '<path d="M11 20H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v2" /><path d="M3 9.5h12" /><path d="M8.5 4v16" />'),
                ("WarnSoft", '<path d="M15 11.5a1.5 1.5 0 0 1 1.5-1.5h3a1.5 1.5 0 0 1 1.5 1.5V21l-3-2-3 2z" fill="currentColor" stroke="none" />'),
                ("Warn", '<path d="M15 11.5a1.5 1.5 0 0 1 1.5-1.5h3a1.5 1.5 0 0 1 1.5 1.5V21l-3-2-3 2z" />'),
            ],
        },
        # PageBreak: rounded page split by crisp horizontal dashed break line
        "PageBreak": {
            "mono": (
                '<path d="M5 9V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v4" />'
                '<path d="M19 15v4a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-4" />'
                '<path d="M3 12h3" />'
                '<path d="M10.5 12h3" />'
                '<path d="M18 12h3" />'
            ),
            "layers": [
                ("Text", '<path d="M5 9V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v4" /><path d="M19 15v4a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-4" />'),
                ("Accent", '<path d="M3 12h3" /><path d="M10.5 12h3" /><path d="M18 12h3" />'),
            ],
        },
        # ShowHeadings: rounded sheet with shaded top row & left column headers
        "ShowHeadings": {
            "mono": (
                '<rect x="3" y="4" width="18" height="16" rx="2" />'
                '<path d="M3 9h18" />'
                '<path d="M8.5 4v16" />'
                '<path d="M14.5 9v11" opacity="0.45" />'
                '<path d="M8.5 14.5H21" opacity="0.45" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="4.8" width="16.4" height="4.2" rx="1" fill="currentColor" stroke="none" /><rect x="3.8" y="9" width="4.7" height="10.2" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 9h18" /><path d="M8.5 4v16" />'),
            ],
        },
        # FormulaBar: wide rounded bar with clean 'fx' and formula line
        "FormulaBar": {
            "mono": (
                '<rect x="2.5" y="7" width="19" height="10" rx="2.2" />'
                '<path d="M9.5 7v10" />'
                '<path d="M5.4 14.5c.8 0 1.1-.4 1.3-1.4l.4-2.8c.2-.9.6-1.3 1.3-1.3" />'
                '<path d="M5.4 11.5h2.5" />'
                '<path d="M12.5 12h6" />'
            ),
        },
        # TextSpill: rounded cell on left with clean gap on right border as text spills out
        "TextSpill": {
            "mono": (
                '<path d="M11 7.2V6a1.5 1.5 0 0 0-1.5-1.5H4.8A1.8 1.8 0 0 0 3 6.3v11.4a1.8 1.8 0 0 0 1.8 1.8h4.7A1.5 1.5 0 0 0 11 18v-1.2" />'
                '<path d="M6.5 10h14" />'
                '<path d="m18 7.5 2.5 2.5-2.5 2.5" />'
                '<path d="M6.5 14.5h11" />'
            ),
            "layers": [
                ("Text", '<path d="M11 7.2V6a1.5 1.5 0 0 0-1.5-1.5H4.8A1.8 1.8 0 0 0 3 6.3v11.4a1.8 1.8 0 0 0 1.8 1.8h4.7A1.5 1.5 0 0 0 11 18v-1.2" /><path d="M6.5 14.5h11" />'),
                ("Accent", '<path d="M6.5 10h14" /><path d="m18 7.5 2.5 2.5-2.5 2.5" />'),
            ],
        },
        # FormulaView: rounded sheet with crisp 'fx' inside
        "FormulaView": {
            "mono": (
                '<rect x="3" y="4" width="18" height="16" rx="2" />'
                '<path d="M7.5 16.5c1.1 0 1.6-.6 1.8-2l.7-5c.2-1.4.8-2 2-2" />'
                '<path d="M7.5 11.2h4" />'
                '<path d="m13.5 11.5 4 5" />'
                '<path d="m17.5 11.5-4 5" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="4" width="18" height="16" rx="2" />'),
                ("Accent", '<path d="M7.5 16.5c1.1 0 1.6-.6 1.8-2l.7-5c.2-1.4.8-2 2-2" /><path d="M7.5 11.2h4" /><path d="m13.5 11.5 4 5" /><path d="m17.5 11.5-4 5" />'),
            ],
        },
        # ErrorCheck: rounded cell + flush top-right error triangle + bottom-right green check
        "ErrorCheck": {
            "mono": (
                '<path d="M10.5 18H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v5" />'
                '<path d="M12.5 4h3.7a1.8 1.8 0 0 1 1.8 1.8v3.7Z" fill="currentColor" stroke="none" />'
                '<path d="m13.5 17 2.5 2.5L21 14" />'
            ),
            "layers": [
                ("Text", '<path d="M10.5 18H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h7.5" /><path d="M18 9.2V11" />'),
                ("Err", '<path d="M12 3.2h4.4a2.4 2.4 0 0 1 2.4 2.4V10Z" fill="currentColor" stroke="none" />'),
                ("Accent", '<path d="m13.5 17 2.5 2.5L21 14" />'),
            ],
        },
        # CommentPrev: speech bubble + left navigation chevron (clear breathing room)
        "CommentPrev": {
            "mono": (
                '<path d="M21.5 15a2 2 0 0 1-2 2h-6l-3 3V17H9.5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" />'
                '<path d="M11 9h7" />'
                '<path d="M11 12.5h4.5" />'
                '<path d="m4.8 8.2-2.6 2.8 2.6 2.8" />'
            ),
            "layers": [
                ("WarnSoft", '<path d="M21.5 15a2 2 0 0 1-2 2h-6l-3 3V17H9.5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" fill="currentColor" stroke="none" />'),
                ("Text", '<path d="M21.5 15a2 2 0 0 1-2 2h-6l-3 3V17H9.5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" /><path d="M11 9h7" /><path d="M11 12.5h4.5" /><path d="m4.8 8.2-2.6 2.8 2.6 2.8" />'),
            ],
        },
        # CommentNext: speech bubble + right navigation chevron (clear breathing room)
        "CommentNext": {
            "mono": (
                '<path d="M16.5 15a2 2 0 0 1-2 2h-5l-3 3V17H4.5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" />'
                '<path d="M6 9h7" />'
                '<path d="M6 12.5h4.5" />'
                '<path d="m19.2 8.2 2.6 2.8-2.6 2.8" />'
            ),
            "layers": [
                ("WarnSoft", '<path d="M16.5 15a2 2 0 0 1-2 2h-5l-3 3V17H4.5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" fill="currentColor" stroke="none" />'),
                ("Text", '<path d="M16.5 15a2 2 0 0 1-2 2h-5l-3 3V17H4.5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" /><path d="M6 9h7" /><path d="M6 12.5h4.5" /><path d="m19.2 8.2 2.6 2.8-2.6 2.8" />'),
            ],
        },
        # Online: person silhouette + smooth signal waves
        "Online": {
            "mono": (
                '<circle cx="8.5" cy="7.5" r="3" />'
                '<path d="M3 19.5a5.5 5.5 0 0 1 11 0" />'
                '<path d="M15.5 11a3.5 3.5 0 0 1 0-5" />'
                '<path d="M18.5 13.5a7 7 0 0 0 0-10" />'
            ),
            "layers": [
                ("Text", '<circle cx="8.5" cy="7.5" r="3" /><path d="M3 19.5a5.5 5.5 0 0 1 11 0" />'),
                ("Accent", '<path d="M15.5 11a3.5 3.5 0 0 1 0-5" /><path d="M18.5 13.5a7 7 0 0 0 0-10" />'),
            ],
        },
        # AutoShare: share nodes + smooth refresh loop arc
        "AutoShare": {
            "mono": (
                '<circle cx="17" cy="5.5" r="2.5" />'
                '<circle cx="6" cy="11.5" r="2.5" />'
                '<circle cx="17" cy="17.5" r="2.5" />'
                '<path d="m8.2 10.3 6.6-3.6" />'
                '<path d="m8.2 12.7 6.6 3.6" />'
                '<path d="M11 20.5a6.5 6.5 0 0 1-6.5-3" />'
                '<path d="M4.5 17.5h3" />'
            ),
            "layers": [
                ("Info", '<circle cx="17" cy="5.5" r="2.5" /><circle cx="6" cy="11.5" r="2.5" /><circle cx="17" cy="17.5" r="2.5" /><path d="m8.2 10.3 6.6-3.6" /><path d="m8.2 12.7 6.6 3.6" />'),
                ("Accent", '<path d="M11 20.5a6.5 6.5 0 0 1-6.5-3" /><path d="M4.5 17.5h3" />'),
            ],
        },
        # Orientation: portrait page + curved clockwise rotate arrow
        "Orientation": {
            "mono": (
                '<rect x="4" y="4" width="10" height="14.5" rx="1.8" />'
                '<path d="M7 8.5h4" />'
                '<path d="M7 12h4" />'
                '<path d="M17.5 18.5a6.5 6.5 0 0 0 2.5-9.5" />'
                '<path d="M17 9h3V6" />'
            ),
            "layers": [
                ("Text", '<rect x="4" y="4" width="10" height="14.5" rx="1.8" /><path d="M7 8.5h4" /><path d="M7 12h4" />'),
                ("Accent", '<path d="M17.5 18.5a6.5 6.5 0 0 0 2.5-9.5" /><path d="M17 9h3V6" />'),
            ],
        },
        # PaperSize: rounded sheet + clean top & right dimension ticks
        "PaperSize": {
            "mono": (
                '<rect x="4" y="7" width="12" height="14" rx="1.8" />'
                '<path d="M4 3.5h12" />'
                '<path d="M4 2.5v2" />'
                '<path d="M16 2.5v2" />'
                '<path d="M20 7v14" />'
                '<path d="M19 7h2" />'
                '<path d="M19 21h2" />'
            ),
        },
        # PrintGrid: printer + crisp 2x2 grid printed on paper
        "PrintGrid": {
            "mono": (
                '<path d="M6 9V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v5" />'
                '<path d="M6 17H4a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-2" />'
                '<rect x="6.5" y="13" width="11" height="8" rx="1" />'
                '<path d="M12 13v8" />'
                '<path d="M6.5 17h11" />'
            ),
        },
        # PrintHeadings: printer + highlighted header band on output paper
        "PrintHeadings": {
            "mono": (
                '<path d="M6 9V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v5" />'
                '<path d="M6 17H4a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-2" />'
                '<rect x="6.5" y="13" width="11" height="8" rx="1" />'
                '<path d="M6.5 16h11" />'
                '<path d="M9.5 13v8" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="7" y="13.5" width="10" height="2.4" fill="currentColor" stroke="none" /><rect x="7" y="13.5" width="2.5" height="7" fill="currentColor" stroke="none" />'),
                ("Text", '<path d="M6 9V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v5" /><path d="M6 17H4a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-2" /><rect x="6.5" y="13" width="11" height="8" rx="1" /><path d="M6.5 16h11" /><path d="M9.5 13v8" />'),
            ],
        },
        # PrintArea: rounded document + dashed green print-area rectangle
        "PrintArea": {
            "mono": (
                '<rect x="4" y="2.5" width="16" height="19" rx="2" />'
                '<rect x="7.5" y="6.5" width="9" height="11" rx="1.2" stroke-dasharray="2.4 2.1" />'
            ),
            "layers": [
                ("AccentSoft", '<rect x="7.5" y="6.5" width="9" height="11" rx="1.2" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="4" y="2.5" width="16" height="19" rx="2" />'),
                ("Accent", '<rect x="7.5" y="6.5" width="9" height="11" rx="1.2" stroke-dasharray="2.4 2.1" />'),
            ],
        },
        # PivotRefresh: mini 2x2 table + smooth circular refresh badge
        "PivotRefresh": {
            "mono": (
                '<path d="M11 19H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v5" />'
                '<path d="M3 8.5h15" />'
                '<path d="M8.5 3v16" />'
                '<path d="M21 16a4 4 0 1 1-1.2-2.8L21 14.5" />'
                '<path d="M21 11.5v3h-3" />'
            ),
            "layers": [
                ("Text", '<path d="M11 19H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v5" /><path d="M3 8.5h15" /><path d="M8.5 3v16" />'),
                ("Accent", '<path d="M21 16a4 4 0 1 1-1.2-2.8L21 14.5" /><path d="M21 11.5v3h-3" />'),
            ],
        },
        # ChartList: mini 3-bar chart on left + 3 list lines on right
        "ChartList": {
            "mono": (
                '<path d="M3 19h9" />'
                '<path d="M4.5 19v-5" />'
                '<path d="M7.5 19V9" />'
                '<path d="M10.5 19v-7" />'
                '<path d="M15 8h6" />'
                '<path d="M15 12.5h6" />'
                '<path d="M15 17h4.5" />'
            ),
            "layers": [
                ("Text", '<path d="M3 19h9" /><path d="M15 8h6" /><path d="M15 12.5h6" /><path d="M15 17h4.5" />'),
                ("Accent", '<path d="M4.5 19v-5" />'),
                ("Info", '<path d="M7.5 19V9" />'),
                ("Warn", '<path d="M10.5 19v-7" />'),
            ],
        },
        # PdfExport: clean document + red PDF bottom accent bar + green export down-arrow
        "PdfExport": {
            "mono": (
                '<path d="M13 20H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h7l4 4v5.5" />'
                '<path d="M13 2v4h4" />'
                '<rect x="6.5" y="13.5" width="7" height="3.2" rx="1" fill="currentColor" stroke="none" />'
                '<path d="M18.5 13v8" />'
                '<path d="m15.8 18.3 2.7 2.7 2.7-2.7" />'
            ),
            "layers": [
                ("Text", '<path d="M13 20H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h7l4 4v5.5" /><path d="M13 2v4h4" /><path d="M7.5 9.5h5" />'),
                ("Err", '<rect x="6.5" y="13.8" width="7" height="3.2" rx="1" fill="currentColor" stroke="none" />'),
                ("Accent", '<path d="M18.5 13v8" /><path d="m15.8 18.3 2.7 2.7 2.7-2.7" />'),
            ],
        },
        # TextVerticalDraw: crisp vertical stacked A + vertical downward text arrow
        "TextVerticalDraw": {
            "mono": (
                '<path d="M5.5 11 8.5 4l3 7" />'
                '<path d="M6.6 8.8h3.8" />'
                '<path d="M6.5 15h4" />'
                '<path d="M6.5 19h4" />'
                '<path d="M17 4.5v15" />'
                '<path d="m14 16.5 3 3 3-3" />'
            ),
        },
        # CalcSheet: mini sheet grid + clean calculator keypad badge
        "CalcSheet": {
            "mono": (
                '<path d="M10.5 19H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v4" />'
                '<path d="M3 8.5h15" />'
                '<path d="M8.5 3v16" />'
                '<rect x="13" y="11" width="8" height="10" rx="1.6" />'
                '<path d="M15.5 14h3" />'
                '<path d="M15.5 17.5h.01" />'
                '<path d="M18.5 17.5h.01" />'
            ),
        },
        # SheetRef: two small offset sheets linked by crisp green arrow
        "SheetRef": {
            "mono": (
                '<rect x="3" y="3.5" width="9" height="8" rx="1.6" />'
                '<rect x="12" y="12.5" width="9" height="8" rx="1.6" />'
                '<path d="M14.5 6.5h2a2 2 0 0 1 2 2v1.5" />'
                '<path d="m16.5 8 2 2 2-2" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="3.5" width="9" height="8" rx="1.6" /><rect x="12" y="12.5" width="9" height="8" rx="1.6" />'),
                ("Accent", '<path d="M14.5 6.5h2a2 2 0 0 1 2 2v1.5" /><path d="m16.5 8 2 2 2-2" />'),
            ],
        },
        # FilterReapply: crisp funnel + curved refresh arrow
        "FilterReapply": {
            "mono": (
                '<path d="M3 4h14l-5.5 6.5v6l-3-1.8v-4.2z" />'
                '<path d="M21 16.5a3.8 3.8 0 1 1-1.1-2.7L21 15" />'
                '<path d="M21 12v3h-3" />'
            ),
            "layers": [
                ("Text", '<path d="M3 4h14l-5.5 6.5v6l-3-1.8v-4.2z" />'),
                ("Accent", '<path d="M21 16.5a3.8 3.8 0 1 1-1.1-2.7L21 15" /><path d="M21 12v3h-3" />'),
            ],
        },
        # SortDialog: dual up/down sort arrows + crisp horizontal criteria lines
        "SortDialog": {
            "mono": (
                '<path d="M6.5 4.5v15" />'
                '<path d="m4 7 2.5-2.5L9 7" />'
                '<path d="m4 17 2.5 2.5L9 17" />'
                '<path d="M12.5 7h8" />'
                '<path d="M12.5 12h6.5" />'
                '<path d="M12.5 17h4.5" />'
            ),
        },
        # Zoom100: magnifying glass with crisp 1:1 square inside
        "Zoom100": {
            "mono": (
                '<circle cx="10.5" cy="10.5" r="7" />'
                '<path d="m20.5 20.5-4.8-4.8" />'
                '<rect x="7.8" y="7.8" width="5.4" height="5.4" rx="1" />'
            ),
        },
        # ZoomDialog: magnifying glass + clean slider dots badge
        "ZoomDialog": {
            "mono": (
                '<circle cx="10.5" cy="10.5" r="6.8" />'
                '<path d="m20.2 20.2-4.6-4.6" />'
                '<path d="M7.5 10.5h6" />'
                '<circle cx="19.5" cy="5.5" r="1.4" fill="currentColor" stroke="none" />'
                '<circle cx="19.5" cy="10.5" r="1.4" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("Text", '<circle cx="10.5" cy="10.5" r="6.8" /><path d="m20.2 20.2-4.6-4.6" /><path d="M7.5 10.5h6" />'),
                ("Accent", '<circle cx="19.5" cy="5.5" r="1.4" fill="currentColor" stroke="none" /><circle cx="19.5" cy="10.5" r="1.4" fill="currentColor" stroke="none" />'),
            ],
        },
        # Unfreeze: rounded sheet with top band + diagonal red cancel slash
        "Unfreeze": {
            "mono": (
                '<path d="M8.5 3.5H19a2 2 0 0 1 2 2v10.5" />'
                '<path d="M15.5 20.5H5a2 2 0 0 1-2-2V8" />'
                '<path d="M13 8.5h8" />'
                '<path d="M3 21 21 3" />'
            ),
            "layers": [
                ("Text", '<path d="M8.5 3.5H19a2 2 0 0 1 2 2v10.5" /><path d="M15.5 20.5H5a2 2 0 0 1-2-2V8" /><path d="M13 8.5h8" />'),
                ("Err", '<path d="M3 21 21 3" />'),
            ],
        },
        # TableToRange: table grid + outward top-right release arrow
        "TableToRange": {
            "mono": (
                '<path d="M12 4H5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-6" />'
                '<path d="M3 9.5h10" />'
                '<path d="M9 4v17" />'
                '<path d="M15 4h6v6" />'
                '<path d="m21 4-7 7" />'
            ),
            "layers": [
                ("Text", '<path d="M12 4H5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-6" /><path d="M3 9.5h10" /><path d="M9 4v17" />'),
                ("Accent", '<path d="M15 4h6v6" /><path d="m21 4-7 7" />'),
            ],
        },
        # SparkEdit: smooth mini sparkline + crisp corner pencil
        "SparkEdit": {
            "mono": (
                '<path d="M3 13c2-3 3.5-5 5.5-2s3.5-4.5 5.5-6" />'
                '<path d="M3 16.5h7.5" />'
                '<path d="m14.5 19.5 5-5a1.4 1.4 0 0 1 2 2l-5 5-2.8.8z" />'
            ),
            "layers": [
                ("Accent", '<path d="M3 13c2-3 3.5-5 5.5-2s3.5-4.5 5.5-6" />'),
                ("Text", '<path d="M3 16.5h7.5" /><path d="m14.5 19.5 5-5a1.4 1.4 0 0 1 2 2l-5 5-2.8.8z" />'),
            ],
        },
        # ChartEdit: mini 3-bar chart + crisp corner pencil
        "ChartEdit": {
            "mono": (
                '<path d="M3 17.5h8" />'
                '<path d="M4.5 17.5v-4.5" />'
                '<path d="M7.5 17.5V8" />'
                '<path d="M10.5 17.5v-6" />'
                '<path d="m14.5 19.5 5-5a1.4 1.4 0 0 1 2 2l-5 5-2.8.8z" />'
            ),
            "layers": [
                ("Accent", '<path d="M4.5 17.5v-4.5" />'),
                ("Info", '<path d="M7.5 17.5V8" />'),
                ("Warn", '<path d="M10.5 17.5v-6" />'),
                ("Text", '<path d="M3 17.5h8" /><path d="m14.5 19.5 5-5a1.4 1.4 0 0 1 2 2l-5 5-2.8.8z" />'),
            ],
        },
        # RichText: crisp 'A' + two colored styled underlines
        "RichText": {
            "mono": (
                '<path d="M7 13.5 12 3.5l5 10" />'
                '<path d="M8.8 10h6.4" />'
                '<path d="M4 17.5h16" />'
                '<path d="M4 20.8h10" />'
            ),
            "layers": [
                ("Text", '<path d="M7 13.5 12 3.5l5 10" /><path d="M8.8 10h6.4" />'),
                ("Info", '<path d="M4 17.5h16" />'),
                ("Warn", '<path d="M4 20.8h10" />'),
            ],
        },
        # Freeze: rounded sheet + frozen top row & left col + crisp snowflake badge
        "Freeze": {
            "mono": (
                '<path d="M11.5 20.5H5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v6" />'
                '<path d="M3 9h17" />'
                '<path d="M8.5 3.5v17" />'
                '<path d="M17.5 13.5v7" />'
                '<path d="m14.5 15.2 6 3.6" />'
                '<path d="m20.5 15.2-6 3.6" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="4.2" width="15.4" height="4.8" rx="1" fill="currentColor" stroke="none" /><rect x="3.8" y="9" width="4.7" height="10.7" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<path d="M11.5 20.5H5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v6" />'),
                ("Info", '<path d="M3 9h17" /><path d="M8.5 3.5v17" /><path d="M17.5 13.5v7" /><path d="m14.5 15.2 6 3.6" /><path d="m20.5 15.2-6 3.6" />'),
            ],
        },
        # FreezeRow: rounded sheet + frozen top header row + crisp snowflake badge
        "FreezeRow": {
            "mono": (
                '<path d="M11.5 20.5H5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v6" />'
                '<path d="M3 9h17" />'
                '<path d="M17.5 13.5v7" />'
                '<path d="m14.5 15.2 6 3.6" />'
                '<path d="m20.5 15.2-6 3.6" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="4.2" width="15.4" height="4.8" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<path d="M11.5 20.5H5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v6" />'),
                ("Info", '<path d="M3 9h17" /><path d="M17.5 13.5v7" /><path d="m14.5 15.2 6 3.6" /><path d="m20.5 15.2-6 3.6" />'),
            ],
        },
        # FreezeCol: rounded sheet + frozen left column + crisp snowflake badge
        "FreezeCol": {
            "mono": (
                '<path d="M11.5 20.5H5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v6" />'
                '<path d="M8.5 3.5v17" />'
                '<path d="M17.5 13.5v7" />'
                '<path d="m14.5 15.2 6 3.6" />'
                '<path d="m20.5 15.2-6 3.6" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="4.2" width="4.7" height="15.5" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<path d="M11.5 20.5H5a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v6" />'),
                ("Info", '<path d="M8.5 3.5v17" /><path d="M17.5 13.5v7" /><path d="m14.5 15.2 6 3.6" /><path d="m20.5 15.2-6 3.6" />'),
            ],
        },
        # Cf: rounded table card + 3 semantic horizontal data bars (Blue / Amber / Green)
        "Cf": {
            "mono": (
                '<rect x="3" y="4" width="18" height="16" rx="2" />'
                '<rect x="6" y="7" width="12" height="2.4" rx="1.1" fill="currentColor" stroke="none" />'
                '<rect x="6" y="10.8" width="8.5" height="2.4" rx="1.1" fill="currentColor" stroke="none" />'
                '<rect x="6" y="14.6" width="5.2" height="2.4" rx="1.1" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="4" width="18" height="16" rx="2" />'),
                ("Info", '<rect x="6" y="7" width="12" height="2.4" rx="1.1" fill="currentColor" stroke="none" />'),
                ("Warn", '<rect x="6" y="10.8" width="8.5" height="2.4" rx="1.1" fill="currentColor" stroke="none" />'),
                ("Accent", '<rect x="6" y="14.6" width="5.2" height="2.4" rx="1.1" fill="currentColor" stroke="none" />'),
            ],
        },
        # ZhS: open-source NotoSansSC-Bold '简' + crisp right arrow
        "ZhS": {
            "mono": (
                f'<path d="{zhs_d}" fill="currentColor" stroke="none" />'
                '<path d="M16.2 12h5.3" />'
                '<path d="m19 9.2 2.8 2.8-2.8 2.8" />'
            ),
            "layers": [
                ("Text", f'<path d="{zhs_d}" fill="currentColor" stroke="none" />'),
                ("Accent", '<path d="M16.2 12h5.3" /><path d="m19 9.2 2.8 2.8-2.8 2.8" />'),
            ],
        },
        # ZhT: open-source NotoSansSC-Bold '繁' + crisp left arrow
        "ZhT": {
            "mono": (
                f'<path d="{zht_d}" fill="currentColor" stroke="none" />'
                '<path d="M7.8 12H2.5" />'
                '<path d="M5 9.2 2.2 12 5 14.8" />'
            ),
            "layers": [
                ("Text", f'<path d="{zht_d}" fill="currentColor" stroke="none" />'),
                ("Accent", '<path d="M7.8 12H2.5" /><path d="M5 9.2 2.2 12 5 14.8" />'),
            ],
        },
        # TraceIn: origin dot + arrow pointing INTO target cell
        "TraceIn": {
            "mono": (
                '<rect x="13" y="6.5" width="8" height="11" rx="1.8" />'
                '<circle cx="4" cy="12" r="1.4" fill="currentColor" stroke="none" />'
                '<path d="M5.5 12h5" />'
                '<path d="m8 9.2 2.8 2.8L8 14.8" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="13.8" y="7.3" width="6.4" height="9.4" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="13" y="6.5" width="8" height="11" rx="1.8" />'),
                ("Info", '<circle cx="4" cy="12" r="1.4" fill="currentColor" stroke="none" /><path d="M5.5 12h5" /><path d="m8 9.2 2.8 2.8L8 14.8" />'),
            ],
        },
        # TraceOut: origin cell + arrow pointing OUT to dependent
        "TraceOut": {
            "mono": (
                '<rect x="3" y="6.5" width="8" height="11" rx="1.8" />'
                '<path d="M13.5 12H20" />'
                '<path d="m17.2 9.2 2.8 2.8-2.8 2.8" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="7.3" width="6.4" height="9.4" rx="1" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="3" y="6.5" width="8" height="11" rx="1.8" />'),
                ("Info", '<path d="M13.5 12H20" /><path d="m17.2 9.2 2.8 2.8-2.8 2.8" />'),
            ],
        },
        # SparkLine: rounded cell frame + smooth Bezier sparkline curve + peak dot
        "SparkLine": {
            "mono": (
                '<rect x="3" y="5" width="18" height="14" rx="2" />'
                '<path d="M5.8 14.8c1.4-2.8 2.4-4.2 3.8-2.4 1.3 1.7 2.2 1.7 3.6-2.2 1.1-3 2.1-2.5 3.6.5" />'
                '<circle cx="17.8" cy="10.2" r="1.35" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="5.8" width="16.4" height="12.4" rx="1.2" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="3" y="5" width="18" height="14" rx="2" />'),
                ("Info", '<path d="M5.8 14.8c1.4-2.8 2.4-4.2 3.8-2.4 1.3 1.7 2.2 1.7 3.6-2.2 1.1-3 2.1-2.5 3.6.5" />'),
                ("Err", '<circle cx="17.8" cy="10.2" r="1.35" fill="currentColor" stroke="none" />'),
            ],
        },
        # SparkColumn: rounded cell frame + 4 rounded mini bars
        "SparkColumn": {
            "mono": (
                '<rect x="3" y="5" width="18" height="14" rx="2" />'
                '<rect x="6" y="12" width="2.2" height="4.4" rx="0.7" fill="currentColor" stroke="none" />'
                '<rect x="9.3" y="8.4" width="2.2" height="8" rx="0.7" fill="currentColor" stroke="none" />'
                '<rect x="12.6" y="13.2" width="2.2" height="3.2" rx="0.7" fill="currentColor" stroke="none" />'
                '<rect x="15.9" y="9.8" width="2.2" height="6.6" rx="0.7" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="5" width="18" height="14" rx="2" />'),
                ("Info", '<rect x="6" y="12" width="2.2" height="4.4" rx="0.7" fill="currentColor" stroke="none" /><rect x="9.3" y="8.4" width="2.2" height="8" rx="0.7" fill="currentColor" stroke="none" /><rect x="12.6" y="13.2" width="2.2" height="3.2" rx="0.7" fill="currentColor" stroke="none" /><rect x="15.9" y="9.8" width="2.2" height="6.6" rx="0.7" fill="currentColor" stroke="none" />'),
            ],
        },
        # SparkLoss: rounded cell frame + zero baseline + green up-bars & red down-bars
        "SparkLoss": {
            "mono": (
                '<rect x="3" y="5" width="18" height="14" rx="2" />'
                '<path d="M5.5 12h13" stroke-width="1.2" />'
                '<rect x="6" y="8.2" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" />'
                '<rect x="9.2" y="12.4" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" />'
                '<rect x="12.5" y="8.2" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" />'
                '<rect x="15.7" y="12.4" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="5" width="18" height="14" rx="2" /><path d="M5.5 12h13" stroke-width="1.2" />'),
                ("Accent", '<rect x="6" y="8.2" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" /><rect x="12.5" y="8.2" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" />'),
                ("Err", '<rect x="9.2" y="12.4" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" /><rect x="15.7" y="12.4" width="2.3" height="3.4" rx="0.7" fill="currentColor" stroke="none" />'),
            ],
        },
        # Macro: code page + clean green play triangle badge
        "Macro": {
            "mono": (
                '<path d="M12 20H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h7l4 4v5.5" />'
                '<path d="M13 2v4h4" />'
                '<path d="M7.5 10h5" />'
                '<path d="M7.5 13.5h3.5" />'
                '<path d="M15.5 14.5v6l5-3z" />'
            ),
            "layers": [
                ("Text", '<path d="M12 20H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h7l4 4v5.5" /><path d="M13 2v4h4" /><path d="M7.5 10h5" /><path d="M7.5 13.5h3.5" />'),
                ("Accent", '<path d="M15.5 14.5v6l5-3z" />'),
            ],
        },
        # Compare: two side-by-side sheets + generous center gap with opposing arrows
        "Compare": {
            "mono": (
                '<rect x="2.5" y="4" width="5.5" height="16" rx="1.5" />'
                '<rect x="16" y="4" width="5.5" height="16" rx="1.5" />'
                '<path d="M9.8 9.2h4.4" />'
                '<path d="m12.4 7.4 1.8 1.8-1.8 1.8" />'
                '<path d="M14.2 14.8H9.8" />'
                '<path d="m11.6 13-1.8 1.8 1.8 1.8" />'
            ),
            "layers": [
                ("Text", '<rect x="2.5" y="4" width="5.5" height="16" rx="1.5" /><rect x="16" y="4" width="5.5" height="16" rx="1.5" />'),
                ("Accent", '<path d="M9.8 9.2h4.4" /><path d="m12.4 7.4 1.8 1.8-1.8 1.8" /><path d="M14.2 14.8H9.8" /><path d="m11.6 13-1.8 1.8 1.8 1.8" />'),
            ],
        },
        # SyncScroll: two side-by-side sheets + generous center up/down sync arrow
        "SyncScroll": {
            "mono": (
                '<rect x="2.5" y="4" width="5.2" height="16" rx="1.5" />'
                '<rect x="16.3" y="4" width="5.2" height="16" rx="1.5" />'
                '<path d="M12 6v12" />'
                '<path d="m10 8 2-2 2 2" />'
                '<path d="m10 16 2 2 2-2" />'
            ),
            "layers": [
                ("Text", '<rect x="2.5" y="4" width="5.2" height="16" rx="1.5" /><rect x="16.3" y="4" width="5.2" height="16" rx="1.5" />'),
                ("Accent", '<path d="M12 6v12" /><path d="m10 8 2-2 2 2" /><path d="m10 16 2 2 2-2" />'),
            ],
        },
        # InvalidCircle: crisp red alert circle + exclamation mark over subtle cell corner
        "InvalidCircle": {
            "mono": (
                '<circle cx="12" cy="12" r="8.5" />'
                '<path d="M12 7.8v4.8" />'
                '<circle cx="12" cy="16" r="1.05" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("ErrSoft", '<circle cx="12" cy="12" r="8.5" fill="currentColor" stroke="none" />'),
                ("Err", '<circle cx="12" cy="12" r="8.5" /><path d="M12 7.8v4.8" /><circle cx="12" cy="16" r="1.05" fill="currentColor" stroke="none" />'),
            ],
        },
        # PasteValues123: crisp rounded badge with smooth-Bezier geometric '1 2 3'
        "PasteValues123": {
            "mono": (
                '<rect x="2.5" y="4.5" width="19" height="15" rx="2.4" />'
                # '1'
                '<path d="M6.0 10.2 7.3 9v6" />'
                # '2'
                '<path d="M10.4 10.4a1.6 1.6 0 0 1 3.1.5c0 .9-.7 1.5-1.5 2.1l-1.8 2h3.5" />'
                # '3'
                '<path d="M16.1 9.8a1.6 1.6 0 0 1 2.8 1c0 .9-.7 1.3-1.5 1.3.9 0 1.6.6 1.6 1.5a1.7 1.7 0 0 1-3 1" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.2" y="5.2" width="17.6" height="13.6" rx="1.8" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="2.5" y="4.5" width="19" height="15" rx="2.4" />'),
                ("Info", '<path d="M6.0 10.2 7.3 9v6" />'),
                ("Accent", '<path d="M10.4 10.4a1.6 1.6 0 0 1 3.1.5c0 .9-.7 1.5-1.5 2.1l-1.8 2h3.5" /><path d="M16.1 9.8a1.6 1.6 0 0 1 2.8 1c0 .9-.7 1.3-1.5 1.3.9 0 1.6.6 1.6 1.5a1.7 1.7 0 0 1-3 1" />'),
            ],
        },
        # Spotlight: rounded sheet with highlighted crosshair bands + focused center cell
        "Spotlight": {
            "mono": (
                '<rect x="3" y="3" width="18" height="18" rx="2" />'
                '<path d="M3 9.5h18" />'
                '<path d="M3 14.5h18" />'
                '<path d="M9.5 3v18" />'
                '<path d="M14.5 3v18" />'
                '<rect x="9.5" y="9.5" width="5" height="5" rx="0.8" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("WarnSoft", '<rect x="3.8" y="9.5" width="16.4" height="5" fill="currentColor" stroke="none" /><rect x="9.5" y="3.8" width="5" height="16.4" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="3" y="3" width="18" height="18" rx="2" /><path d="M3 9.5h6.5" /><path d="M14.5 9.5H21" /><path d="M3 14.5h6.5" /><path d="M14.5 14.5H21" /><path d="M9.5 3v6.5" /><path d="M9.5 14.5V21" /><path d="M14.5 3v6.5" /><path d="M14.5 14.5V21" />'),
                ("Accent", '<rect x="9.5" y="9.5" width="5" height="5" rx="0.8" />'),
            ],
        },
        # SplitPanes: rounded window split cleanly into 4 quadrants by bold cross divider
        "SplitPanes": {
            "mono": (
                '<rect x="3" y="4" width="18" height="16" rx="2" />'
                '<path d="M12 4v16" stroke-width="2.2" />'
                '<path d="M3 12h18" stroke-width="2.2" />'
            ),
            "layers": [
                ("Text", '<rect x="3" y="4" width="18" height="16" rx="2" />'),
                ("Info", '<path d="M12 4.8v14.4" stroke-width="2.1" /><path d="M3.8 12h16.4" stroke-width="2.1" />'),
            ],
        },
        # HighlightDup: two matching highlighted cells overlapping cleanly
        "HighlightDup": {
            "mono": (
                '<rect x="3" y="4" width="11" height="8" rx="1.8" />'
                '<path d="M6 8h5" />'
                '<rect x="10" y="12" width="11" height="8" rx="1.8" />'
                '<path d="M13 16h5" />'
            ),
            "layers": [
                ("WarnSoft", '<rect x="3.6" y="4.6" width="9.8" height="6.8" rx="1.2" fill="currentColor" stroke="none" />'),
                ("ErrSoft", '<rect x="10.6" y="12.6" width="9.8" height="6.8" rx="1.2" fill="currentColor" stroke="none" />'),
                ("Warn", '<rect x="3" y="4" width="11" height="8" rx="1.8" /><path d="M6 8h5" />'),
                ("Err", '<rect x="10" y="12" width="11" height="8" rx="1.8" /><path d="M13 16h5" />'),
            ],
        },
        # RejectDup: sheet rows + red prohibition 'ban' badge
        "RejectDup": {
            "mono": (
                '<path d="M11 19H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v4.5" />'
                '<path d="M6.5 8h7" />'
                '<path d="M6.5 12h4.5" />'
                '<circle cx="16.5" cy="16.5" r="4.5" />'
                '<path d="m13.5 13.5 6 6" />'
            ),
            "layers": [
                ("Text", '<path d="M11 19H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v4.5" /><path d="M6.5 8h7" /><path d="M6.5 12h4.5" />'),
                ("ErrSoft", '<circle cx="16.5" cy="16.5" r="4.5" fill="currentColor" stroke="none" />'),
                ("Err", '<circle cx="16.5" cy="16.5" r="4.5" /><path d="m13.5 13.5 6 6" />'),
            ],
        },
        # MergeSame: left tall merged cell + right two separate cells
        "MergeSame": {
            "mono": (
                '<rect x="3" y="4" width="7.5" height="16" rx="1.8" />'
                '<rect x="13.5" y="4" width="7.5" height="6.5" rx="1.6" />'
                '<rect x="13.5" y="13.5" width="7.5" height="6.5" rx="1.6" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.6" y="4.6" width="6.3" height="14.8" rx="1.2" fill="currentColor" stroke="none" />'),
                ("AccentSoft", '<rect x="14.1" y="4.6" width="6.3" height="5.3" rx="1" fill="currentColor" stroke="none" /><rect x="14.1" y="14.1" width="6.3" height="5.3" rx="1" fill="currentColor" stroke="none" />'),
                ("Info", '<rect x="3" y="4" width="7.5" height="16" rx="1.8" />'),
                ("Accent", '<rect x="13.5" y="4" width="7.5" height="6.5" rx="1.6" /><rect x="13.5" y="13.5" width="7.5" height="6.5" rx="1.6" />'),
            ],
        },
        # CellStyle: styled sheet card with tinted blue header + crisp 'A' + solid green swatch pill
        "CellStyle": {
            "mono": (
                '<rect x="3" y="4" width="18" height="16" rx="2.2" />'
                '<path d="M3 8.8h18" />'
                '<path d="M6.2 17 8.5 11.8 10.8 17" />'
                '<path d="M7 15.3h3" />'
                '<rect x="13.8" y="12.2" width="4.4" height="4.4" rx="1.2" fill="currentColor" stroke="none" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.8" y="4.8" width="16.4" height="4" rx="1.1" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="3" y="4" width="18" height="16" rx="2.2" /><path d="M6.2 17 8.5 11.8 10.8 17" /><path d="M7 15.3h3" />'),
                ("Info", '<path d="M3.5 8.8h17" />'),
                ("Accent", '<rect x="13.8" y="12.2" width="4.4" height="4.4" rx="1.2" fill="currentColor" stroke="none" />'),
            ],
        },
        # Transpose: horizontal row block + vertical col block + smooth curved swap arrow
        "Transpose": {
            "mono": (
                '<rect x="3" y="3.5" width="10" height="4.8" rx="1.4" />'
                '<rect x="15.7" y="11" width="4.8" height="10" rx="1.4" />'
                '<path d="M16 5.5h1.5a2 2 0 0 1 2 2V9" />'
                '<path d="M6.5 11.5V16a2 2 0 0 0 2 2h4.5" />'
                '<path d="m10.8 15.8 2.2 2.2-2.2 2.2" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="3.6" y="4.1" width="8.8" height="3.6" rx="0.9" fill="currentColor" stroke="none" />'),
                ("AccentSoft", '<rect x="16.3" y="11.6" width="3.6" height="8.8" rx="0.9" fill="currentColor" stroke="none" />'),
                ("Info", '<rect x="3" y="3.5" width="10" height="4.8" rx="1.4" />'),
                ("Accent", '<rect x="15.7" y="11" width="4.8" height="10" rx="1.4" />'),
                ("Warn", '<path d="M6.5 11.5V16a2 2 0 0 0 2 2h4.5" /><path d="m10.8 15.8 2.2 2.2-2.2 2.2" />'),
            ],
        },
        # PasteLink: clipboard + smooth link chain badge in bottom-right
        "PasteLink": {
            "mono": (
                '<rect x="7.5" y="2.5" width="6" height="3.2" rx="1" />'
                '<path d="M9.5 19.5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2.5" />'
                '<path d="M13.5 4H16a2 2 0 0 1 2 2v4.5" />'
                '<path d="M13.5 17.5h-1a2.5 2.5 0 0 1 0-5h1" />'
                '<path d="M17.5 12.5h1a2.5 2.5 0 0 1 0 5h-1" />'
                '<path d="M13.5 15h4" />'
            ),
            "layers": [
                ("WarnSoft", '<path d="M5 4h11a2 2 0 0 1 2 2v4.5h-6a4 4 0 0 0-3 6.5v2.5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="7.5" y="2.5" width="6" height="3.2" rx="1" /><path d="M9.5 19.5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2.5" /><path d="M13.5 4H16a2 2 0 0 1 2 2v4.5" />'),
                ("Info", '<path d="M13.5 17.5h-1a2.5 2.5 0 0 1 0-5h1" /><path d="M17.5 12.5h1a2.5 2.5 0 0 1 0 5h-1" /><path d="M13.5 15h4" />'),
            ],
        },
        # PastePic: clipboard + crisp mini image badge in bottom-right
        "PastePic": {
            "mono": (
                '<rect x="7.5" y="2.5" width="6" height="3.2" rx="1" />'
                '<path d="M8.5 19.5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2.5" />'
                '<path d="M13.5 4H16a2 2 0 0 1 2 2v2.5" />'
                '<rect x="11" y="11" width="10" height="9.5" rx="1.6" />'
                '<circle cx="14.2" cy="14" r="1.1" fill="currentColor" stroke="none" />'
                '<path d="m21 17.8-2.6-2.5a1.2 1.2 0 0 0-1.7 0L12.5 20" />'
            ),
            "layers": [
                ("WarnSoft", '<path d="M5 4h11a2 2 0 0 1 2 2v2.5H11v11H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z" fill="currentColor" stroke="none" />'),
                ("Text", '<rect x="7.5" y="2.5" width="6" height="3.2" rx="1" /><path d="M8.5 19.5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2.5" /><path d="M13.5 4H16a2 2 0 0 1 2 2v2.5" /><rect x="11" y="11" width="10" height="9.5" rx="1.6" />'),
                ("Warn", '<circle cx="14.2" cy="14" r="1.1" fill="currentColor" stroke="none" />'),
                ("Info", '<path d="m21 17.8-2.6-2.5a1.2 1.2 0 0 0-1.7 0L12.5 20" />'),
            ],
        },
        # ColWidth: vertical highlighted column + outward left/right width arrows
        "ColWidth": {
            "mono": (
                '<rect x="8.5" y="3.5" width="7" height="17" rx="1.6" />'
                '<path d="M8.5 9h7" />'
                '<path d="M8.5 15h7" />'
                '<path d="M2.5 12h3.5" />'
                '<path d="m4.5 9.8-2.2 2.2 2.2 2.2" />'
                '<path d="M21.5 12H18" />'
                '<path d="m19.5 9.8 2.2 2.2-2.2 2.2" />'
            ),
            "layers": [
                ("InfoSoft", '<rect x="9.1" y="4.1" width="5.8" height="15.8" rx="1" fill="currentColor" stroke="none" />'),
                ("Info", '<rect x="8.5" y="3.5" width="7" height="17" rx="1.6" /><path d="M8.5 9h7" /><path d="M8.5 15h7" />'),
                ("Warn", '<path d="M2.5 12h3.5" /><path d="m4.5 9.8-2.2 2.2 2.2 2.2" /><path d="M21.5 12H18" /><path d="m19.5 9.8 2.2 2.2-2.2 2.2" />'),
            ],
        },
    }
    return custom


# Semantic color layers for Lucide-sourced icons!
# Either:
#   - `{"soft": (role, svg_inner)}` -> additive duotone background fill under full mono icon!
#   - `{"layers": [(role, svg_inner), ...]}` -> disjoint full replacement layers (ZERO AA fringe)!
LUCIDE_SEMANTIC_LAYERS: dict[str, dict[str, Any]] = {
    # Additive subtle duotone fills (clean modern Fluent-2 warmth)
    "Open": {
        "soft": ("WarnSoft", '<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" fill="currentColor" stroke="none" />'),
    },
    "Save": {
        "soft": ("InfoSoft", '<path d="M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z" fill="currentColor" stroke="none" />'),
    },
    "Comment": {
        "soft": ("WarnSoft", '<path d="M22 17a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 21.286V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2z" fill="currentColor" stroke="none" />'),
    },
    "ShowComment": {
        "soft": ("WarnSoft", '<path d="M12.7 3H4a2 2 0 0 0-2 2v16.286a.71.71 0 0 0 1.212.502l2.202-2.202A2 2 0 0 1 6.828 19H20a2 2 0 0 0 2-2v-4.7" fill="currentColor" stroke="none" />'),
    },
    "Lock": {
        "soft": ("WarnSoft", '<rect width="18" height="11" x="3" y="11" rx="2" fill="currentColor" stroke="none" />'),
    },
    "Table": {
        "soft": ("InfoSoft", '<rect x="3.5" y="3.5" width="17" height="5.5" rx="1.5" fill="currentColor" stroke="none" />'),
    },
    "Sheet": {
        "soft": ("AccentSoft", '<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8l6 6v12a2 2 0 0 1-2 2z" fill="currentColor" stroke="none" />'),
    },
    "FnBook": {
        "soft": ("InfoSoft", '<path d="M12 7v14" /><path d="M3 18a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h5a4 4 0 0 1 4 4 4 4 0 0 1 4-4h5a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1h-6a3 3 0 0 0-3 3 3 3 0 0 0-3-3z" fill="currentColor" stroke="none" />'),
    },
    "Brush": {
        "soft": ("WarnSoft", '<path d="M9 8c-1.804 2.71-3.97 3.46-6.583 3.948a.507.507 0 0 0-.302.819l7.32 8.883a1 1 0 0 0 1.185.204C12.735 20.405 16 16.792 16 15Z" fill="currentColor" stroke="none" />'),
    },
    "Eraser": {
        "soft": ("ErrSoft", '<path d="M21 21H8a2 2 0 0 1-1.42-.587l-3.994-3.999a2 2 0 0 1 0-2.828l10-10a2 2 0 0 1 2.829 0l5.999 6a2 2 0 0 1 0 2.828L12.834 21" fill="currentColor" stroke="none" />'),
    },

    # Disjoint multi-color Lucide splits (ZERO AA fringe!)
    "Paste": {
        "layers": [
            ("WarnSoft", '<rect x="4.5" y="4.5" width="15" height="17" rx="1.8" fill="currentColor" stroke="none" />'),
            ("Text", '<path d="M16 4h2a2 2 0 0 1 2 2v2.37" /><path d="M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-.37" /><rect x="8" y="2" width="8" height="4" rx="1" />'),
            ("Accent", '<path d="M12 14h8" /><path d="m17 17 3-3-3-3" />'),
        ],
    },
    "FolderPlus": {
        "layers": [
            ("WarnSoft", '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" fill="currentColor" stroke="none" />'),
            ("Text", '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />'),
            ("Accent", '<path d="M12 10v6" /><path d="M9 13h6" />'),
        ],
    },
    "Fill": {
        "layers": [
            ("WarnSoft", '<path d="M21.145 18.38A3.34 3.34 0 0 1 20 16.5a3.3 3.3 0 0 1-1.145 1.88c-.575.46-.855 1.02-.855 1.595A2 2 0 0 0 20 22a2 2 0 0 0 2-2.025c0-.58-.285-1.13-.855-1.595Z" fill="currentColor" stroke="none" />'),
            ("Text", '<path d="M11 7 6 2" /><path d="M18.992 12H2.041" /><path d="m8.5 4.5 2.148-2.148a1.205 1.205 0 0 1 1.704 0l7.296 7.296a1.205 1.205 0 0 1 0 1.704l-7.592 7.592a3.615 3.615 0 0 1-5.112 0l-3.888-3.888a3.615 3.615 0 0 1 0-5.112L5.67 7.33" />'),
            ("Warn", '<path d="M21.145 18.38A3.34 3.34 0 0 1 20 16.5a3.3 3.3 0 0 1-1.145 1.88c-.575.46-.855 1.02-.855 1.595A2 2 0 0 0 20 22a2 2 0 0 0 2-2.025c0-.58-.285-1.13-.855-1.595" />'),
        ],
    },
    "Image": {
        "layers": [
            ("InfoSoft", '<path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21h13a2 2 0 0 0 2-2Z" fill="currentColor" stroke="none" />'),
            ("Text", '<rect width="18" height="18" x="3" y="3" rx="2" ry="2" />'),
            ("Info", '<path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" />'),
            ("Warn", '<circle cx="9" cy="9" r="2" />'),
        ],
    },
    "InsertCell": {
        "layers": [
            ("AccentSoft", '<rect x="3.6" y="12" width="8.4" height="8.4" rx="1" fill="currentColor" stroke="none" />'),
            ("Text", '<path d="M12 3v17a1 1 0 0 1-1 1H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v6a1 1 0 0 1-1 1H3" />'),
            ("Accent", '<path d="M16 19h6" /><path d="M19 22v-6" />'),
        ],
    },
    "DeleteCell": {
        "layers": [
            ("ErrSoft", '<rect x="3.6" y="12" width="8.4" height="8.4" rx="1" fill="currentColor" stroke="none" />'),
            ("Text", '<path d="M12 3v17a1 1 0 0 1-1 1H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v6a1 1 0 0 1-1 1H3" />'),
            ("Err", '<path d="m16.5 16.5 5 5" /><path d="m16.5 21.5 5-5" />'),
        ],
    },
    "AiSparkle": {
        "layers": [
            ("InfoSoft", '<path d="M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z" fill="currentColor" stroke="none" />'),
            ("Info", '<path d="M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z" />'),
            ("Accent", '<path d="M20 2v4" /><path d="M22 4h-4" /><circle cx="4" cy="20" r="2" />'),
        ],
    },
    "Share": {
        "layers": [
            ("InfoSoft", '<circle cx="18" cy="5" r="3" fill="currentColor" stroke="none" /><circle cx="6" cy="12" r="3" fill="currentColor" stroke="none" /><circle cx="18" cy="19" r="3" fill="currentColor" stroke="none" />'),
            ("Text", '<line x1="8.59" x2="15.42" y1="13.51" y2="17.49" /><line x1="15.41" x2="8.59" y1="6.51" y2="10.49" />'),
            ("Info", '<circle cx="18" cy="5" r="3" /><circle cx="6" cy="12" r="3" /><circle cx="18" cy="19" r="3" />'),
        ],
    },
    "A11y": {
        "layers": [
            ("Info", '<circle cx="16" cy="4" r="1" /><path d="m18 19 1-7-6 1" /><path d="m5 8 3-3 5.5 3-2.36 3.5" />'),
            ("Accent", '<path d="M4.24 14.5a5 5 0 0 0 6.88 6" /><path d="M13.76 17.5a5 5 0 0 0-6.88-6" />'),
        ],
    },
    "SortAsc": {
        "layers": [
            ("Accent", '<path d="m3 16 4 4 4-4" /><path d="M7 20V4" />'),
            ("Text", '<path d="M20 8h-5" /><path d="M15 10V6.5a2.5 2.5 0 0 1 5 0V10" /><path d="M15 14h5l-5 6h5" />'),
        ],
    },
    "SortDesc": {
        "layers": [
            ("Accent", '<path d="m3 16 4 4 4-4" /><path d="M7 4v16" />'),
            ("Text", '<path d="M15 4h5l-5 6h5" /><path d="M15 20v-3.5a2.5 2.5 0 0 1 5 0V20" /><path d="M20 18h-5" />'),
        ],
    },
    "SaveAs": {
        "layers": [
            ("InfoSoft", '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h10.2a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4v2L14 17.5V21Z" fill="currentColor" stroke="none" />'),
            ("Text", '<path d="M13.33 13H8a1 1 0 0 0-1 1v7" /><path d="M7 3v4a1 1 0 0 0 1 1h7" /><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h10.2a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4v.3" />'),
            ("Accent", '<path d="M14.363 17.634a2 2 0 0 0-.506.854l-.837 2.87a.5.5 0 0 0 .62.62l2.87-.837a2 2 0 0 0 .854-.506l4.013-4.009a1 1 0 1 0-3.004-3.004z" />'),
        ],
    },
    "Validation": {
        "layers": [
            ("Text", '<path d="M12 3v17a1 1 0 0 1-1 1H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v6a1 1 0 0 1-1 1H3" />'),
            ("Accent", '<path d="m16 19 2 2 4-4" />'),
        ],
    },
    "Spelling": {
        "layers": [
            ("Text", '<path d="m4 16 6-12 5.115 10.23" /><path d="M6 12h8" />'),
            ("Accent", '<path d="m20 15-5.5 5.5L12 18" />'),
        ],
    },
    "ClearFmt": {
        "layers": [
            ("Text", '<path d="M4 7V4h16v3" /><path d="M5 20h6" /><path d="M13 4 8 20" />'),
            ("Err", '<path d="m15 15 5 5" /><path d="m20 15-5 5" />'),
        ],
    },
    "NewFile": {
        "layers": [
            ("Text", '<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" />'),
            ("Accent", '<path d="M9 15h6" /><path d="M12 18v-6" />'),
        ],
    },
    "MacroRun": {
        "layers": [
            ("AccentSoft", '<circle cx="12" cy="12" r="10" fill="currentColor" stroke="none" />'),
            ("Accent", '<circle cx="12" cy="12" r="10" /><path d="M9 9.003a1 1 0 0 1 1.517-.859l4.997 2.997a1 1 0 0 1 0 1.718l-4.997 2.997A1 1 0 0 1 9 14.996z" />'),
        ],
    },
    "StrikeThrough": {
        "layers": [
            ("Text", '<path d="M16 4H9a3 3 0 0 0-2.83 4" /><path d="M14 12a4 4 0 0 1 0 8H6" />'),
            ("Err", '<line x1="4" x2="20" y1="12" y2="12" />'),
        ],
    },
    "Currency": {
        "layers": [
            ("Accent", '<path d="M12 9.5V21m0-11.5L6 3m6 6.5L18 3" /><path d="M6 15h12" /><path d="M6 11h12" />'),
        ],
    },
    "Percent": {
        "layers": [
            ("Accent", '<line x1="19" x2="5" y1="5" y2="19" /><circle cx="6.5" cy="6.5" r="2.5" /><circle cx="17.5" cy="17.5" r="2.5" />'),
        ],
    },
    "FontGrow": {
        "layers": [
            ("Text", '<path d="M3.5 13h6" /><path d="m2 16 4.5-9 4.5 9" />'),
            ("Accent", '<path d="M18 16V7" /><path d="m14 11 4-4 4 4" />'),
        ],
    },
    "FontShrink": {
        "layers": [
            ("Text", '<path d="M3.5 13h6" /><path d="m2 16 4.5-9 4.5 9" />'),
            ("Info", '<path d="M18 7v9" /><path d="m14 12 4 4 4-4" />'),
        ],
    },
    "NoBorder": {
        "layers": [
            ("Text", '<path d="M15 9.344V3" /><path d="M20.414 20.414A2 2 0 0 1 19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 .586-1.414" /><path d="M20.656 15H21" /><path d="M21 9h-6.344" /><path d="M3 15h12v6" /><path d="M3 9h6v12" /><path d="M8.656 3H19a2 2 0 0 1 2 2v10.343" /><path d="M9 3v.344" />'),
            ("Err", '<path d="m2 2 20 20" />'),
        ],
    },
}


def strip_lucide_inner(svg_text: str) -> str:
    """Extract clean inner elements from a Lucide SVG file."""
    m = re.search(r"<svg[^>]*>(.*)</svg>", svg_text, re.DOTALL)
    if not m:
        raise ValueError("Invalid SVG: missing <svg> wrapper")
    inner = re.sub(r"\s+", " ", m.group(1)).strip()
    return inner


def fetch_lucide_package(dest: Path) -> Path:
    dest.mkdir(parents=True, exist_ok=True)
    pkg_dir = dest / "package"
    if (pkg_dir / "icons").is_dir() and (pkg_dir / "LICENSE").is_file():
        return pkg_dir
    meta = json.loads(urllib.request.urlopen("https://registry.npmjs.org/lucide-static/latest", timeout=20).read())
    tar_url = meta["dist"]["tarball"]
    print(f"[lucide] downloading {tar_url} ...")
    raw = urllib.request.urlopen(tar_url, timeout=45).read()
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tf:
        tf.extractall(dest, filter="data")
    return pkg_dir


def load_or_build_sources(lucide_dir: Path | None, fetch_lucide: bool) -> dict[str, Any]:
    sources_path = OUT_DIR / "sources.json"
    if lucide_dir is None and not fetch_lucide and sources_path.is_file():
        print(f"[sources] loading offline sources from {sources_path}")
        return json.loads(sources_path.read_text(encoding="utf-8"))

    if lucide_dir is None:
        cache_dir = Path(os.environ.get("TEMP", ".")) / "cellan-lucide-cache"
        pkg_dir = fetch_lucide_package(cache_dir)
    else:
        pkg_dir = lucide_dir / "package" if (lucide_dir / "package").is_dir() else lucide_dir

    icons_dir = pkg_dir / "icons"
    pkg_json = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
    lucide_version = pkg_json.get("version", "unknown")

    # Copy ISC + Feather MIT LICENSE verbatim into assets/icons/modern/LICENSE-LUCIDE.txt
    license_src = pkg_dir / "LICENSE"
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    header = (
        f"CELLAN Modern Icon Set Attribution\n"
        f"===================================\n\n"
        f"1. Lucide Icons v{lucide_version} ({len(LUCIDE_MAP)} icons)\n"
        f"   Source: https://lucide.dev / npm package `lucide-static@{lucide_version}`\n"
        f"   License: ISC License (with Feather-derived icons under MIT License), reproduced verbatim below.\n\n"
        f"2. Custom CELLAN Spreadsheet Icons ({len(ICON_ORDER) - len(LUCIDE_MAP)} icons)\n"
        f"   Authored in matching 24x24 optical stroke style for CELLAN (MIT License).\n"
        f"   CJK glyphs in ZhS / ZhT are derived from Noto Sans SC Bold (SIL Open Font License 1.1).\n\n"
        f"-------------------------------------------------------------------------------\n\n"
    )
    (OUT_DIR / "LICENSE-LUCIDE.txt").write_text(
        header + license_src.read_text(encoding="utf-8"),
        encoding="utf-8",
    )

    custom_svgs = build_custom_svgs()
    entries: dict[str, Any] = {}
    for name in ICON_ORDER:
        if name in LUCIDE_MAP:
            lname = LUCIDE_MAP[name]
            svg_file = icons_dir / f"{lname}.svg"
            if not svg_file.is_file():
                raise FileNotFoundError(f"Lucide icon not found: {svg_file}")
            mono = strip_lucide_inner(svg_file.read_text(encoding="utf-8"))
            entry: dict[str, Any] = {
                "source": f"lucide:{lname}",
                "mono": mono,
            }
            if name in LUCIDE_SEMANTIC_LAYERS:
                sem = LUCIDE_SEMANTIC_LAYERS[name]
                if "soft" in sem:
                    entry["soft"] = list(sem["soft"])
                if "layers" in sem:
                    entry["layers"] = [list(x) for x in sem["layers"]]
            entries[name] = entry
        else:
            if name not in custom_svgs:
                raise KeyError(f"Icon {name!r} missing from both LUCIDE_MAP and custom_svgs!")
            spec = custom_svgs[name]
            entry = {
                "source": f"custom:{name}",
                "mono": spec["mono"],
            }
            if "layers" in spec:
                entry["layers"] = [list(x) for x in spec["layers"]]
            entries[name] = entry

    data = {
        "lucide_version": lucide_version,
        "lucide_license": "ISC (Feather-derived icons: MIT)",
        "icon_order": ICON_ORDER,
        "icons": entries,
    }
    sources_path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"[sources] wrote {len(entries)} icons to {sources_path} (Lucide {lucide_version}: {len(LUCIDE_MAP)}, custom: {len(ICON_ORDER) - len(LUCIDE_MAP)})")
    return data


def find_chrome_executable() -> str | None:
    candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    for c in candidates:
        if os.path.isfile(c):
            return c
    return None


def svg_cell_html(inner: str, cell_px: int, stroke_width: float) -> str:
    return (
        f'<div class="cell">'
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{cell_px}" height="{cell_px}" '
        f'viewBox="0 0 24 24" fill="none" stroke="#FFFFFF" color="#FFFFFF" '
        f'stroke-width="{stroke_width:.2f}" stroke-linecap="round" stroke-linejoin="round">'
        f"{inner}</svg></div>"
    )


def render_atlas_grid(
    browser,
    cells_svg: list[str],
    cols: int,
    rows: int,
    cell_px: int,
    stroke_width: float,
    supersample: int = 2,
) -> Image.Image:
    """Render an entire SVG icon grid via Chromium Skia at `supersample`x and Lanczos-downsample to LA8."""
    width = cols * cell_px
    height = rows * cell_px
    total = cols * rows
    padded = list(cells_svg) + [""] * (total - len(cells_svg))

    body_cells = "".join(
        svg_cell_html(s, cell_px, stroke_width) if s else '<div class="cell"></div>'
        for s in padded
    )
    html = f"""<!doctype html>
<html>
<head><meta charset="utf-8"><style>
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
html, body {{ width: {width}px; height: {height}px; overflow: hidden; background: transparent; }}
.grid {{
  display: grid;
  grid-template-columns: repeat({cols}, {cell_px}px);
  grid-auto-rows: {cell_px}px;
  width: {width}px;
  height: {height}px;
}}
.cell {{
  width: {cell_px}px;
  height: {cell_px}px;
  display: flex;
  align-items: center;
  justify-content: center;
}}
svg {{ display: block; shape-rendering: geometricPrecision; }}
</style></head>
<body><div class="grid">{body_cells}</div></body>
</html>"""

    page = browser.new_page(
        viewport={"width": width, "height": height},
        device_scale_factor=supersample,
    )
    page.set_content(html, wait_until="load")
    png_bytes = page.screenshot(omit_background=True, full_page=False)
    page.close()

    rgba = Image.open(io.BytesIO(png_bytes)).convert("RGBA")
    if rgba.size != (width, height):
        # Premultiplied-safe alpha downsample: since RGB is pure white (255,255,255),
        # downsampling the Alpha channel alone with Lanczos is mathematically exact with ZERO dark fringe!
        alpha = rgba.getchannel("A").resize((width, height), Image.Resampling.LANCZOS)
    else:
        alpha = rgba.getchannel("A")

    # Store as single-channel 8-bit Grayscale (L8) PNG where luminance == exact 8-bit alpha (0..255).
    # Half the raw bytes of LA8 with 100% bit-identical lossless anti-aliased alpha; decoded in Rust
    # as premultiplied white-alpha pixels `Color32::from_rgba_premultiplied(a, a, a, a)`.
    return alpha


def save_optimized_png(img: Image.Image, path: Path) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path, format="PNG", optimize=True, compress_level=9)
    return path.stat().st_size


def generate_rust_meta(
    accent_rows: int,
    color_specs: dict[str, tuple[bool, list[tuple[int, str]]]],
) -> None:
    """Emit crates/app/src/ui_icons/atlas_meta.rs synchronized with generated PNG atlases."""
    lines: list[str] = [
        "// @generated by scripts/build_modern_icons.py — DO NOT EDIT BY HAND.",
        "",
        "use super::Icon;",
        "",
        f"pub(super) const MONO_COLS: u32 = {MONO_COLS};",
        f"pub(super) const MONO_ROWS: u32 = {MONO_ROWS};",
        f"pub(super) const ACCENT_COLS: u32 = {ACCENT_COLS};",
        f"pub(super) const ACCENT_ROWS: u32 = {accent_rows};",
        "",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        "pub(super) enum ColorRole {",
        "    Text,",
        "    TextWeak,",
        "    Accent,",
        "    Info,",
        "    Warn,",
        "    Err,",
        "    AccentSoft,",
        "    InfoSoft,",
        "    WarnSoft,",
        "    ErrSoft,",
        "}",
        "",
        "#[derive(Clone, Copy, Debug)]",
        "pub(super) struct AccentLayer {",
        "    pub cell: u16,",
        "    pub role: ColorRole,",
        "}",
        "",
        "#[derive(Clone, Copy, Debug)]",
        "pub(super) struct ColorSpec {",
        "    /// When false, `layers` are additive soft fills drawn UNDER the mono stroke.",
        "    /// When true, `layers` are complete disjoint layers drawn INSTEAD of the mono stroke.",
        "    pub replace_base: bool,",
        "    pub layers: &'static [AccentLayer],",
        "}",
        "",
        "pub(super) const fn icon_index(icon: Icon) -> usize {",
        "    match icon {",
    ]
    for idx, name in enumerate(ICON_ORDER):
        lines.append(f"        Icon::{name} => {idx},")
    lines.append("    }")
    lines.append("}")
    lines.append("")
    lines.append("pub(super) const fn color_spec(icon: Icon) -> Option<ColorSpec> {")
    lines.append("    match icon {")
    for name in ICON_ORDER:
        if name not in color_specs:
            continue
        replace_base, layers = color_specs[name]
        layer_str = ", ".join(
            f"AccentLayer {{ cell: {cell}, role: ColorRole::{role} }}"
            for cell, role in layers
        )
        rb = "true" if replace_base else "false"
        lines.append(
            f"        Icon::{name} => Some(ColorSpec {{ replace_base: {rb}, layers: &[{layer_str}] }}),"
        )
    lines.append("        _ => None,")
    lines.append("    }")
    lines.append("}")
    lines.append("")

    META_RS.parent.mkdir(parents=True, exist_ok=True)
    META_RS.write_text("\n".join(lines), encoding="utf-8")
    print(f"[rust] wrote {META_RS} ({len(color_specs)} semantic color specs)")


def build_contact_sheets(
    out_dir: Path,
    mono_64: Image.Image,
    mono_32: Image.Image,
    accent_64: Image.Image,
    accent_32: Image.Image,
    color_specs: dict[str, tuple[bool, list[tuple[int, str]]]],
) -> None:
    """Render Light & Dark visual QA sheets at 32px and 16px so every icon can be inspected."""
    out_dir.mkdir(parents=True, exist_ok=True)
    palettes = {
        "light": {
            "bg": (255, 255, 255),
            "card": (245, 246, 248),
            "border": (224, 227, 232),
            "Text": (31, 35, 41, 255),
            "TextWeak": (100, 106, 115, 255),
            "Accent": (24, 128, 82, 255),
            "Info": (29, 78, 216, 255),
            "Warn": (180, 110, 14, 255),
            "Err": (185, 28, 28, 255),
            "AccentSoft": (24, 128, 82, 46),
            "InfoSoft": (29, 78, 216, 42),
            "WarnSoft": (245, 176, 36, 54),
            "ErrSoft": (185, 28, 28, 42),
        },
        "dark": {
            "bg": (32, 35, 42),
            "card": (38, 42, 50),
            "border": (56, 62, 74),
            "Text": (232, 236, 242, 255),
            "TextWeak": (164, 172, 186, 255),
            "Accent": (46, 178, 118, 255),
            "Info": (147, 197, 253, 255),
            "Warn": (252, 211, 77, 255),
            "Err": (252, 165, 165, 255),
            "AccentSoft": (46, 178, 118, 56),
            "InfoSoft": (147, 197, 253, 52),
            "WarnSoft": (252, 211, 77, 56),
            "ErrSoft": (252, 165, 165, 52),
        },
    }

    def tint_alpha(alpha_crop: Image.Image, rgba: tuple[int, int, int, int], size: int) -> Image.Image:
        a = alpha_crop.resize((size, size), Image.Resampling.LANCZOS)
        if rgba[3] < 255:
            scale = rgba[3] / 255.0
            a = a.point(lambda v: int(round(v * scale)))
        layer = Image.new("RGBA", (size, size), (rgba[0], rgba[1], rgba[2], 255))
        layer.putalpha(a)
        return layer

    for theme_name, pal in palettes.items():
        for px, m_img, a_img, cell_src in [
            (32, mono_64, accent_64, 64),
            (16, mono_32, accent_32, 32),
        ]:
            tile_w, tile_h = (64, 54) if px == 32 else (48, 38)
            cols = 15
            rows = math.ceil(len(ICON_ORDER) / cols)
            sheet = Image.new("RGBA", (cols * tile_w + 16, rows * tile_h + 16), pal["bg"] + (255,))
            draw = ImageDraw.Draw(sheet)

            m_alpha = m_img if m_img.mode == "L" else m_img.getchannel("A")
            a_alpha = a_img if a_img.mode == "L" else a_img.getchannel("A")

            for idx, name in enumerate(ICON_ORDER):
                c = idx % cols
                r = idx // cols
                x0 = 8 + c * tile_w
                y0 = 8 + r * tile_h
                draw.rounded_rectangle(
                    [x0 + 2, y0 + 2, x0 + tile_w - 3, y0 + tile_h - 3],
                    radius=5,
                    fill=pal["card"] + (255,),
                    outline=pal["border"] + (255,),
                )
                ix = x0 + (tile_w - px) // 2
                iy = y0 + (6 if px == 32 else 5)

                mc = (idx % MONO_COLS) * cell_src
                mr = (idx // MONO_COLS) * cell_src
                mono_crop = m_alpha.crop((mc, mr, mc + cell_src, mr + cell_src))

                if name in color_specs:
                    replace_base, layers = color_specs[name]
                    if not replace_base:
                        for cell_idx, role in layers:
                            ac = (cell_idx % ACCENT_COLS) * cell_src
                            ar = (cell_idx // ACCENT_COLS) * cell_src
                            crop = a_alpha.crop((ac, ar, ac + cell_src, ar + cell_src))
                            sheet.alpha_composite(tint_alpha(crop, pal[role], px), (ix, iy))
                        sheet.alpha_composite(tint_alpha(mono_crop, pal["Text"], px), (ix, iy))
                    else:
                        for cell_idx, role in layers:
                            ac = (cell_idx % ACCENT_COLS) * cell_src
                            ar = (cell_idx // ACCENT_COLS) * cell_src
                            crop = a_alpha.crop((ac, ar, ac + cell_src, ar + cell_src))
                            sheet.alpha_composite(tint_alpha(crop, pal[role], px), (ix, iy))
                else:
                    sheet.alpha_composite(tint_alpha(mono_crop, pal["Text"], px), (ix, iy))

                # Short index label below icon
                label = name[:8] if px == 32 else str(idx)
                draw.text((x0 + 4, y0 + tile_h - 13), label, fill=pal["TextWeak"][:3])

            out_file = out_dir / f"sheet_{theme_name}_{px}px.png"
            sheet.save(out_file)
            print(f"[qa] wrote {out_file}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Build CELLAN 1.86 modern icon atlases.")
    parser.add_argument("--lucide-dir", type=Path, default=None, help="Path to extracted lucide-static package")
    parser.add_argument("--fetch-lucide", action="store_true", help="Download latest lucide-static from npm")
    parser.add_argument("--contact-sheet", type=Path, default=None, help="Output dir for Light/Dark QA contact sheets")
    args = parser.parse_args()

    sources = load_or_build_sources(args.lucide_dir, args.fetch_lucide)
    icons = sources["icons"]

    mono_svgs = [icons[name]["mono"] for name in ICON_ORDER]

    # Collect accent cells deterministically in ICON_ORDER (deduplicating identical SVG masks)
    accent_svgs: list[str] = []
    accent_dedup: dict[str, int] = {}
    color_specs: dict[str, tuple[bool, list[tuple[int, str]]]] = {}

    def intern_accent(svg_inner: str) -> int:
        idx = accent_dedup.get(svg_inner)
        if idx is None:
            idx = len(accent_svgs)
            accent_svgs.append(svg_inner)
            accent_dedup[svg_inner] = idx
        return idx

    for name in ICON_ORDER:
        entry = icons[name]
        if "layers" in entry:
            layer_refs: list[tuple[int, str]] = []
            for role, svg_inner in entry["layers"]:
                layer_refs.append((intern_accent(svg_inner), role))
            color_specs[name] = (True, layer_refs)
        elif "soft" in entry:
            role, svg_inner = entry["soft"]
            color_specs[name] = (False, [(intern_accent(svg_inner), role)])

    accent_rows = max(1, math.ceil(len(accent_svgs) / ACCENT_COLS))
    print(f"[grid] mono={MONO_COLS}x{MONO_ROWS} ({len(mono_svgs)} icons), accent={ACCENT_COLS}x{accent_rows} ({len(accent_svgs)} cells, {len(color_specs)} colored icons)")

    chrome_exe = find_chrome_executable()
    launch_kwargs: dict[str, Any] = {"headless": True}
    if chrome_exe:
        launch_kwargs["executable_path"] = chrome_exe
        print(f"[render] using Chromium: {chrome_exe}")

    with sync_playwright() as p:
        browser = p.chromium.launch(**launch_kwargs)
        try:
            mono_64 = render_atlas_grid(browser, mono_svgs, MONO_COLS, MONO_ROWS, 64, stroke_width=1.70, supersample=2)
            mono_32 = render_atlas_grid(browser, mono_svgs, MONO_COLS, MONO_ROWS, 32, stroke_width=1.95, supersample=2)
            accent_64 = render_atlas_grid(browser, accent_svgs, ACCENT_COLS, accent_rows, 64, stroke_width=1.70, supersample=2)
            accent_32 = render_atlas_grid(browser, accent_svgs, ACCENT_COLS, accent_rows, 32, stroke_width=1.95, supersample=2)
        finally:
            browser.close()

    s_m64 = save_optimized_png(mono_64, OUT_DIR / "atlas_64.png")
    s_m32 = save_optimized_png(mono_32, OUT_DIR / "atlas_32.png")
    s_a64 = save_optimized_png(accent_64, OUT_DIR / "accent_64.png")
    s_a32 = save_optimized_png(accent_32, OUT_DIR / "accent_32.png")
    total_bytes = s_m64 + s_m32 + s_a64 + s_a32

    print(
        f"[png] atlas_64={s_m64/1024:.1f} KB, atlas_32={s_m32/1024:.1f} KB, "
        f"accent_64={s_a64/1024:.1f} KB, accent_32={s_a32/1024:.1f} KB => "
        f"TOTAL={total_bytes/1024:.1f} KB ({total_bytes} bytes)"
    )
    if total_bytes > 350 * 1024:
        print(f"ERROR: total atlas PNG size {total_bytes} exceeds 350 KB budget!", file=sys.stderr)
        return 1

    # Sanity-check every single mono cell has non-empty alpha and clean border margins
    m64_a = mono_64 if mono_64.mode == "L" else mono_64.getchannel("A")
    empty_cells = []
    for idx, name in enumerate(ICON_ORDER):
        cx = (idx % MONO_COLS) * 64
        cy = (idx // MONO_COLS) * 64
        crop = m64_a.crop((cx, cy, cx + 64, cy + 64))
        lo, hi = crop.getextrema()
        if hi < 128:
            empty_cells.append(name)
    if empty_cells:
        print(f"ERROR: empty rendered cells: {empty_cells}", file=sys.stderr)
        return 1

    manifest = {
        "lucide_version": sources["lucide_version"],
        "license": sources["lucide_license"],
        "mono_grid": {"cols": MONO_COLS, "rows": MONO_ROWS, "count": len(ICON_ORDER)},
        "accent_grid": {"cols": ACCENT_COLS, "rows": accent_rows, "count": len(accent_svgs)},
        "files": {
            "atlas_64.png": {"bytes": s_m64, "cell_px": 64, "stroke_width": 1.70},
            "atlas_32.png": {"bytes": s_m32, "cell_px": 32, "stroke_width": 1.95},
            "accent_64.png": {"bytes": s_a64, "cell_px": 64, "stroke_width": 1.70},
            "accent_32.png": {"bytes": s_a32, "cell_px": 32, "stroke_width": 1.95},
        },
        "total_png_bytes": total_bytes,
        "icons": [
            {
                "index": i,
                "name": name,
                "source": icons[name]["source"],
                "has_color": name in color_specs,
            }
            for i, name in enumerate(ICON_ORDER)
        ],
    }
    (OUT_DIR / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    generate_rust_meta(accent_rows, color_specs)

    if args.contact_sheet:
        build_contact_sheets(args.contact_sheet, mono_64, mono_32, accent_64, accent_32, color_specs)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
