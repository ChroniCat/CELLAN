#!/usr/bin/env python3
"""QA-01 三方矩阵 runner（manifest 驱动）。

对 manifest 里每个夹具：
  1) 原文件在 Excel / WPS 各取一份快照（真值基线）；
  2) 走我们的 load+save 生成副本；
  3) 副本在 Excel / WPS 各取一份快照；
  4) 逐类别比对「原文件 vs 副本」，把差异记成 diff，绝不把没比对到的写成通过。

快照覆盖：工作表/取值、条件格式、数据验证、表格、名称、冻结窗格、隐藏行列、
图片与形状（类型/名字/尺寸）、图表（系列/标题）、外部链接、透视表（字段/方向）。
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import win32com.client

from qa01_roundtrip import app_name, sha256

ROOT = Path(__file__).resolve().parent.parent


def _safe(fn, default=None):
    try:
        return fn()
    except Exception:
        return default


def snapshot(product: str, path: Path) -> dict:
    app = None
    book = None
    try:
        app = win32com.client.DispatchEx(app_name(product))
        app.Visible = False
        _safe(lambda: setattr(app, "DisplayAlerts", False))
        book = app.Workbooks.Open(str(path), ReadOnly=True, UpdateLinks=0)
        sheets = []
        for si in range(1, int(book.Worksheets.Count) + 1):
            sh = book.Worksheets(si)
            info: dict = {"name": str(sh.Name)}
            info["used"] = str(_safe(lambda: sh.UsedRange.Address(0, 0), ""))
            info["cf"] = int(_safe(lambda: sh.Cells.FormatConditions.Count, 0))
            info["dv"] = int(_safe(lambda: sh.Cells.Validation.Count, 0))
            tables = []
            for ti in range(1, int(_safe(lambda: sh.ListObjects().Count, 0)) + 1):
                t = sh.ListObjects(ti)
                tables.append({"name": str(t.Name), "range": str(t.Range.Address(0, 0))})
            info["tables"] = tables
            info["pictures"] = int(_safe(lambda: sh.Pictures().Count, 0))
            shapes = []
            for i in range(1, int(_safe(lambda: sh.Shapes.Count, 0)) + 1):
                s = sh.Shapes(i)
                item = {
                    "type": int(_safe(lambda: s.Type, 0)),
                    "name": str(_safe(lambda: s.Name, "")),
                    "w": int(_safe(lambda: s.Width, 0)),
                    "h": int(_safe(lambda: s.Height, 0)),
                }
                if item["type"] == 6:
                    item["items"] = int(_safe(lambda: s.GroupItems.Count, 0))
                if bool(_safe(lambda: s.HasSmartArt, False)):
                    item["smartart_nodes"] = int(_safe(lambda: s.SmartArt.Nodes.Count, 0))
                shapes.append(item)
            info["shapes"] = shapes
            charts = []
            for ci in range(1, int(_safe(lambda: sh.ChartObjects().Count, 0)) + 1):
                co = sh.ChartObjects(ci)
                charts.append({
                    "name": str(_safe(lambda: co.Name, "")),
                    "series": int(_safe(lambda: co.Chart.SeriesCollection().Count, 0)),
                    "title": str(_safe(lambda: co.Chart.ChartTitle.Text, "")),
                })
            info["charts"] = charts
            pivots = []
            for pi in range(1, int(_safe(lambda: sh.PivotTables().Count, 0)) + 1):
                pt = sh.PivotTables(pi)
                fields = []
                for fi in range(1, int(_safe(lambda: pt.PivotFields().Count, 0)) + 1):
                    f = pt.PivotFields(fi)
                    fields.append({"name": str(f.Name), "orientation": int(_safe(lambda: f.Orientation, 0))})
                pivots.append({"name": str(pt.Name), "fields": fields})
            info["pivots"] = pivots
            info["page"] = {
                "orientation": int(_safe(lambda: sh.PageSetup.Orientation, 0)),
                "paper": int(_safe(lambda: sh.PageSetup.PaperSize, 0)),
                "zoom": int(_safe(lambda: sh.PageSetup.Zoom, 0)),
                "print_area": str(_safe(lambda: sh.PageSetup.PrintArea, "")),
                "title_rows": str(_safe(lambda: sh.PageSetup.PrintTitleRows, "")),
                "title_cols": str(_safe(lambda: sh.PageSetup.PrintTitleColumns, "")),
            }
            info["freeze"] = {
                "split_row": int(_safe(lambda: sh.Parent.Windows(1).SplitRow, 0)),
                "split_col": int(_safe(lambda: sh.Parent.Windows(1).SplitColumn, 0)),
            }
            info["hidden_rows"] = [
                r for r in range(1, 41) if bool(_safe(lambda r=r: sh.Rows(r).Hidden, False))
            ]
            info["hidden_cols"] = [
                c for c in range(1, 21) if bool(_safe(lambda c=c: sh.Columns(c).Hidden, False))
            ]
            sheets.append(info)
        names = {}
        for ni in range(1, int(_safe(lambda: book.Names.Count, 0)) + 1):
            n = book.Names(ni)
            key = str(n.Name)
            if key.startswith("_sc_"):
                # 我们自己的隐藏行记账名，不算用户可见的名称
                continue
            names[key] = str(_safe(lambda: n.RefersTo, ""))
        return {
            "product": product,
            "version": str(_safe(lambda: app.Version, "")),
            "file": str(path),
            "sha256": sha256(path),
            "sheets": sheets,
            "names": names,
            "links": sorted(Path(x).name for x in _links(book)),
        }
    finally:
        if book is not None:
            _safe(lambda: book.Close(False))
        if app is not None:
            _safe(lambda: app.Quit())


def _links(book) -> list[str]:
    raw = _safe(lambda: book.LinkSources(1))
    if raw is None:
        return []
    if isinstance(raw, str):
        return [raw]
    try:
        return [str(x) for x in raw]
    except TypeError:
        return [str(raw)]


IGNORE_KEYS = {"product", "version", "file", "sha256"}


def diff(a: dict, b: dict, path: str = "") -> list[str]:
    out: list[str] = []
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if not path and k in IGNORE_KEYS:
                continue
            if k not in a:
                out.append(f"{path}.{k}: 副本多出 {b[k]!r}")
            elif k not in b:
                out.append(f"{path}.{k}: 副本缺少 {a[k]!r}")
            else:
                out += diff(a[k], b[k], f"{path}.{k}" if path else k)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path}: 长度 {len(a)} != {len(b)}")
        for i in range(min(len(a), len(b))):
            out += diff(a[i], b[i], f"{path}[{i}]")
    else:
        if a != b:
            out.append(f"{path}: {a!r} != {b!r}")
    return out


def cargo_bin() -> str:
    found = shutil.which("cargo")
    if found:
        return found
    for cand in (Path.home() / ".cargo" / "bin" / "cargo.exe", Path.home() / ".cargo" / "bin" / "cargo"):
        if cand.exists():
            return str(cand)
    return "cargo"


def our_roundtrip(src: Path, dst: Path) -> tuple[bool, str]:
    # 目标已存在时我们的保存会走「就地补丁」，旧文件里的锚点等部件会被沿用，
    # 看起来像回归（chart-ext 实测：残留的 twoCellAnchor 让 WPS 一直读到 202）。
    # 每次从干净目标开始，保证测的是「源文件 → 新文件」这条路径。
    if dst.exists():
        dst.unlink()
    cmd = [
        cargo_bin(), "run", "-q", "-p", "sheetcore", "--offline", "-j", "2",
        "--example", "pivot_roundtrip", "--", str(src), str(dst),
    ]
    env = dict(os.environ)
    env["PATH"] = str(Path(cargo_bin()).parent) + os.pathsep + env.get("PATH", "")
    proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)
    ok = proc.returncode == 0 and dst.exists()
    return ok, (proc.stdout + proc.stderr).strip().splitlines()[-1] if (proc.stdout or proc.stderr) else ""


def excel_resave(path: Path) -> tuple[bool, str]:
    """让 Excel 打开我们的副本并原样保存（模拟「第三方刷新后重存」）"""
    ps = (
        "$ErrorActionPreference='Stop';"
        "$xl=New-Object -ComObject Excel.Application;"
        "$xl.Visible=$false;$xl.DisplayAlerts=$false;"
        f"$wb=$xl.Workbooks.Open('{path.as_posix()}');"
        "$wb.Save();$wb.Close($false);$xl.Quit();"
        "'OK'"
    )
    try:
        out = subprocess.run(
            ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", ps],
            capture_output=True, text=True, timeout=300,
        )
        if out.returncode != 0:
            return False, (out.stderr or out.stdout or "").strip()[:400]
        return True, "ok"
    except Exception as e:  # noqa: BLE001
        return False, f"{type(e).__name__}: {e}"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default=str(ROOT / "scripts" / "qa01_manifest.json"))
    ap.add_argument("--out", default=str(ROOT / "work" / "probe" / "qa01" / "matrix-result.json"))
    ap.add_argument("--only", default="")
    ap.add_argument("--resave", action="store_true", help="再让 Excel 打开我们的副本保存一次，然后复测")
    args = ap.parse_args()

    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    entries = manifest["entries"]
    if args.only:
        want = {x.strip() for x in args.only.split(",") if x.strip()}
        entries = [e for e in entries if e["name"] in want]

    tmp_dir = ROOT / "work" / "probe" / "qa01" / "matrix"
    tmp_dir.mkdir(parents=True, exist_ok=True)
    results = []
    for entry in entries:
        src = ROOT / entry["file"]
        dst = tmp_dir / f"{entry['name']}-ours{src.suffix}"
        rec: dict = {"name": entry["name"], "categories": entry.get("categories", []), "file": entry["file"]}
        if not src.exists():
            rec["error"] = f"缺夹具 {src}"
            results.append(rec)
            print(f"[{entry['name']}] 缺夹具")
            continue
        ok, note = our_roundtrip(src, dst)
        rec["roundtrip"] = {"ok": ok, "note": note}
        if not ok:
            results.append(rec)
            print(f"[{entry['name']}] 我们的往返失败：{note}")
            continue
        for product in ("excel", "wps"):
            base = snapshot(product, src)
            ours = snapshot(product, dst)
            d = diff(base, ours)
            known_keys = entry.get("known_loss", [])
            known = [x for x in d if any(k in x for k in known_keys)]
            unexpected = [x for x in d if x not in known]
            rec[product] = {
                "checked": entry.get("categories", []),
                "diffs": unexpected,
                "known_loss": known,
            }
            print(f"[{entry['name']}] {product}: 意外 {len(unexpected)} 处 / 已登记损失 {len(known)} 处")
            for line in unexpected[:8]:
                print("   !", line)
            for line in known[:8]:
                print("   ~", line)
        if args.resave:
            rs, rnote = excel_resave(dst)
            rec["resave"] = {"excel_ok": rs, "note": rnote}
            if rs:
                dst2 = tmp_dir / f"{entry['name']}-resaved{src.suffix}"
                ok2, note2 = our_roundtrip(dst, dst2)
                rec["resave"]["our_roundtrip_ok"] = ok2
                rec["resave"]["note2"] = note2
                if ok2:
                    vs_orig, vs_resaved = {}, {}
                    for product in ("excel", "wps"):
                        base = snapshot(product, src)
                        mid = snapshot(product, dst)      # Excel 重存后的中间态
                        again = snapshot(product, dst2)   # 我们基于中间态再存的结果
                        vs_orig[product] = diff(base, again)
                        vs_resaved[product] = diff(mid, again)
                    rec["resave"]["diffs"] = vs_orig            # 兼容旧字段：相对原文件
                    rec["resave"]["diffs_vs_resaved"] = vs_resaved
                    print(
                        f"[{entry['name']}] resave: vs原文件 excel {len(vs_orig['excel'])} / wps {len(vs_orig['wps'])}；"
                        f"vs Excel重存 excel {len(vs_resaved['excel'])} / wps {len(vs_resaved['wps'])}"
                    )
                else:
                    print(f"[{entry['name']}] resave 后我们的往返失败：{note2}")
            else:
                print(f"[{entry['name']}] resave 失败：{rnote}")
        results.append(rec)

    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "manifest": str(args.manifest),
        "products": {"excel": "Excel.Application", "wps": "ket.Application"},
        "results": results,
    }
    Path(args.out).write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    def _bad(r):
        if r.get("error") or not r.get("roundtrip", {}).get("ok"):
            return True
        if any(r.get(p, {}).get("diffs") for p in ("excel", "wps")):
            return True
        rv = r.get("resave") or {}
        if rv and (not rv.get("excel_ok") or not rv.get("our_roundtrip_ok")):
            return True
        return any((rv.get("diffs_vs_resaved") or {}).get(p) for p in ("excel", "wps"))

    bad = [r for r in results if _bad(r)]
    print(f"entries={len(results)} with_diffs={len(bad)} -> {args.out}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
