# -*- coding: utf-8 -*-
"""把 assets/fonts 里的原始字体裁成子集：GB2312 全字符 + 拉丁 + 常用符号。

用法：uv run --with fonttools python scripts/subset_fonts.py
"""
import glob
import io
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(HERE, "assets", "fonts")
OUT = os.path.join(HERE, "assets", "fonts", "subset")

FONTS = [
    ("NotoSansSC-Regular.ttf", "NotoSansSC-Regular.subset.ttf"),
    ("NotoSerifSC-Regular.ttf", "NotoSerifSC-Regular.subset.ttf"),
    ("Inter-Regular.ttf", "Inter-Regular.subset.ttf"),
    ("JetBrainsMono-Regular.ttf", "JetBrainsMono-Regular.subset.ttf"),
]


def charset() -> str:
    chars = set()
    for hi in range(0xA1, 0xF8):
        for lo in range(0xA1, 0xFF):
            try:
                chars.add(bytes([hi, lo]).decode("gb2312"))
            except UnicodeDecodeError:
                pass
    for c in range(0x20, 0x7F):
        chars.add(chr(c))
    chars.update("　、。〈〉《》「」『』【】〔〕・…—～￥·×÷±≤≥≠∞√∑∏∫≈°′″℃№")
    # 不换行空格：Excel 常用   做对齐，字体缺了会显示成问号
    chars.update("     ")
    chars.update("①②③④⑤⑥⑦⑧⑨⑩ⅠⅡⅢⅣⅤ")
    # 三角形（功能区下拉箭头用 ▾，缺字形会显示成方框）
    chars.update("▾▼▸◀▶◁▷●○◆◇■□▲△")
    # 源码里真正用到的字符全部纳入：GB2312 之外还有 ¥（半角人民币）、−（U+2212 真减号）这类，
    # 漏掉就会在界面上渲染成方框。直接扫 *.rs 的字符串字面量，比手工维护清单可靠。
    lit = re.compile(r'"((?:[^"\\]|\\.)*)"')
    for path in glob.glob(
        os.path.join(HERE, "crates", "**", "src", "**", "*.rs"), recursive=True
    ):
        try:
            src = io.open(path, encoding="utf-8").read()
        except OSError:
            continue
        for m in lit.finditer(src):
            for ch in m.group(1):
                if ord(ch) >= 0x20 and ch != "\\":
                    chars.add(ch)
    return "".join(sorted(chars))


def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    text = charset()
    text_file = os.path.join(OUT, "charset.txt")
    with open(text_file, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"字符集 {len(text)} 个字符")
    for src, dst in FONTS:
        s = os.path.join(SRC, src)
        d = os.path.join(OUT, dst)
        if not os.path.exists(s):
            print(f"跳过（缺文件）{src}")
            continue
        cmd = [
            sys.executable, "-m", "fontTools.subset", s,
            f"--text-file={text_file}",
            f"--output-file={d}",
            "--layout-features=*",
            "--glyph-names",
            "--no-hinting",
        ]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            print(f"失败 {src}: {r.stderr[-300:]}")
            continue
        print(f"{src} {os.path.getsize(s)//1024} KB -> {dst} {os.path.getsize(d)//1024} KB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
