#!/usr/bin/env python3
"""QA-09 机械验收：PDF/打印真实产物（本程序 vs Excel/WPS 导出）。

对每个夹具：
- 原始文件 sha256 前后不变（导出不改原值）
- 页数/页面尺寸、页眉页脚页码、重复标题
- 每个数据 token 只出现一次（重复标题除外）；无缺行/重复行
- 词框不出页面、词框重叠（文字切断/重叠的机械代理）
- 正文在页边距内（页脚在边距内单独放行）
- 只嵌 Embedded/EmbeddedBold TrueType，不依赖系统字体
- 与 WPS/Excel 导出的 PDF 做文字集合与页数对比

用法（需要 PyMuPDF + openpyxl）：
  uv run --with pymupdf --with openpyxl --with pillow python scripts/qa09_verify.py
"""
import hashlib
import itertools
import json
import os
import re
import subprocess
import sys

import pymupdf

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "target", "debug", "app.exe")
OUT = os.path.join(ROOT, "work", "probe", "qa09")
os.makedirs(OUT, exist_ok=True)


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def export_app(xlsx, pdf):
    env = dict(os.environ, SC_EXPORT_PDF=pdf, SC_EXPORT_PDF_EXIT="1", SC_ANIMATION_TIME="0")
    p = subprocess.run([APP, xlsx], capture_output=True, text=True, env=env, timeout=180)
    assert os.path.exists(pdf), p.stdout + p.stderr
    return [l for l in (p.stdout + p.stderr).splitlines() if l.startswith("[pdf]")]


def norm(s):
    return re.sub(r"\s+", "", s)


def check(pdf, name):
    d = pymupdf.open(pdf)
    res = {"file": name, "pages": d.page_count, "page_size": [round(v, 1) for v in d[0].rect]}
    words, texts = [], []
    for pg in d:
        ws = pg.get_text("words")
        words.append(ws)
        texts.append(norm(pg.get_text()))
    res["texts"] = texts
    res["word_pages"] = words

    outside, overlaps = 0, 0
    for pg, ws in zip(d, words):
        r = pg.rect
        for w in ws:
            if w[0] < -1 or w[1] < -1 or w[2] > r.width + 1 or w[3] > r.height + 1:
                outside += 1
        for a, b in itertools.combinations(ws, 2):
            ax0, ay0, ax1, ay1 = a[:4]
            bx0, by0, bx1, by1 = b[:4]
            iw = min(ax1, bx1) - max(ax0, bx0)
            ih = min(ay1, by1) - max(ay0, by0)
            if iw <= 0 or ih <= 0:
                continue
            inter = iw * ih
            small = min((ax1 - ax0) * (ay1 - ay0), (bx1 - bx0) * (by1 - by0))
            if small > 0 and inter / small > 0.5 and inter > 25:
                overlaps += 1
    res["words_outside_page"] = outside
    res["overlapping_word_pairs"] = overlaps

    fonts = []
    for pg in d:
        for f in pg.get_fonts():
            fonts.append((f[3], f[2], f[1]))
    res["fonts"] = sorted(set(fonts))
    res["non_embedded"] = [f for f in res["fonts"] if f[1] != "ttf"]

    # 页码 & 页脚
    res["page_labels"] = []
    for i, pg in enumerate(d):
        t = norm(pg.get_text())
        has_footer = ("页" in t and ("第" in t or "/" in t)) 
        res["page_labels"].append((f"{i + 1}/{d.page_count}" in t or f"第{i + 1}页" in t) if has_footer else None)

    # 正文边距：排除页脚（y > 页高-页脚区）
    setup = None
    res["body_inside_margins"] = True
    if words:
        left = min((w[0] for ws in words for w in ws), default=0)
        right = max((w[2] for ws in words for w in ws), default=0)
        body = [w for ws in words for w in ws if w[3] < d[0].rect.height - 40]
        if body:
            res["body_x_range"] = [round(min(w[0] for w in body), 1), round(max(w[2] for w in body), 1)]
            res["body_inside_margins"] = res["body_x_range"][0] >= 30 and res["body_x_range"][1] <= d[0].rect.width - 30
        res["ink_x_range"] = [round(left, 1), round(right, 1)]
    return res, d


def tokens_once(word_pages, pattern, expect, allow_repeat=()):
    seen = {}
    for i, ws in enumerate(word_pages):
        for w in ws:
            tok = w[4]
            if re.fullmatch(pattern, tok):
                seen.setdefault(tok, []).append(i)
    missing = [e for e in expect if e not in seen]
    dup = {k: v for k, v in seen.items() if len(v) > 1 and k not in allow_repeat}
    return {"missing": missing[:10], "duplicates": dict(list(dup.items())[:10]), "distinct": len(seen)}


def main():
    report = {}
    fixtures = {
        "print": "crates/core/tests/fixtures/c16/print.xlsx",
        "objects": "crates/core/tests/fixtures/c16/objects.xlsx",
        "number_formats": "crates/core/tests/fixtures/c16/number_formats.xlsx",
        "wrap": "crates/core/tests/fixtures/c16/wrap.xlsx",
        "hidden": "crates/core/tests/fixtures/qa09/hidden.xlsx",
        "fit_width": "crates/core/tests/fixtures/qa09/fit_width.xlsx",
    }
    for key, rel in fixtures.items():
        xlsx = os.path.join(ROOT, rel)
        pdf = os.path.join(OUT, f"app_{key}.pdf")
        before = sha(xlsx)
        log = export_app(xlsx, pdf)
        after = sha(xlsx)
        res, d = check(pdf, rel)
        res["export_log"] = log
        res["source_sha_unchanged"] = before == after
        if key == "print":
            expect = [f"行{i}" for i in range(10, 61)]
            res["row_tokens"] = tokens_once(res["word_pages"], r"行\d+", expect)
            res["titles_each_page"] = [("2026年第一季度销售汇总" in t) for t in res["texts"]]
            res["page_num_each_page"] = [f"第{i+1}页" in t or f"{i+1}/2" in t for i, t in enumerate(res["texts"])]
            res["row_split"] = [
                [m.group(0) for m in re.finditer(r"行\d+", t)][:2] for t in res["texts"]
            ]
        if key == "hidden":
            joined = " ".join(res["texts"])
            res["hidden_absent"] = all(x not in joined for x in
                                       ["隐藏列B内容", "行5隐藏行内容", "C5隐藏", "行7筛选隐藏", "C7隐藏", "B7隐藏"])
            res["visible_present"] = all(x in joined for x in
                                         ["可见列A", "可见列C", "行3可见", "行6可见", "行8可见"])
        if key == "objects":
            pg0 = d[0]
            res["images_page1"] = [[round(v, 1) for v in pg0.get_image_rects(i[0])[0]]
                                   for i in pg0.get_images(full=True)]
            res["drawings_page1"] = len(d[0].get_drawings())
            res["textbox_present"] = "打印文本框" in res["texts"][0]
        if key == "fit_width":
            ws = [w for w in d[0].get_text("words") if w[4] == "第18列标题"]
            res["last_col_present"] = bool(ws)
            res["last_col_x"] = round(ws[0][0], 1) if ws else None
        report[key] = {k: v for k, v in res.items() if k not in ("texts", "word_pages")}
        report[key]["text_len"] = [len(t) for t in res["texts"]]

    # 三方文字对比（有 Excel/WPS 导出的才比）
    peers = {
        "print": ("work/probe/wps_print.pdf", "work/probe/excel_print.pdf"),
        "objects": ("work/probe/wps_obj.pdf", "work/probe/excel_obj.pdf"),
        "hidden": (None, "work/probe/excel_hidden.pdf"),
        "fit_width": (None, "work/probe/excel_fit.pdf"),
    }
    for key, (wps, excel) in peers.items():
        app_pdf = os.path.join(OUT, f"app_{key}.pdf")
        app_doc = pymupdf.open(app_pdf)
        app_text = norm("".join(pg.get_text() for pg in app_doc))
        item = {}
        for label, p in (("wps", wps), ("excel", excel)):
            if not p or not os.path.exists(os.path.join(ROOT, p)):
                continue
            dd = pymupdf.open(os.path.join(ROOT, p))
            t = norm("".join(pg.get_text() for pg in dd))
            from collections import Counter
            app_words = Counter(w[4] for pg in app_doc for w in pg.get_text("words"))
            peer_words = Counter(w[4] for pg in dd for w in pg.get_text("words"))
            item[label] = {
                "pages": dd.page_count,
                "text_equal": t == app_text,
                "len": [len(app_text), len(t)],
                "same_char_ratio": round(sum(1 for a, b in zip(app_text, t) if a == b) / max(1, max(len(app_text), len(t))), 3),
                "words_missing_in_pdf": sum((app_words - peer_words).values()),
                "words_extra_in_pdf": sum((peer_words - app_words).values()),
                "sample_missing": list((app_words - peer_words).items())[:5],
                "sample_extra": list((peer_words - app_words).items())[:5],
            }
        report[key]["peers"] = item

    path = os.path.join(OUT, "report.json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=1)
    print(json.dumps(report, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    sys.exit(main())
