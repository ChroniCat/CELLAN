# -*- coding: utf-8 -*-
"""把 assets/fonts 里的原始字体裁成子集：GB2312 全字符 + 拉丁 + 常用符号。

用法：uv run --with fonttools python scripts/subset_fonts.py
仅重建 UI 500 Medium 子集：python scripts/subset_fonts.py --medium-only
"""
import glob
import io
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(HERE, "assets", "fonts")
OUT = os.path.join(HERE, "assets", "fonts", "subset")

FONTS = [
    ("NotoSansSC-Regular.ttf", "NotoSansSC-Regular.subset.ttf"),
    ("NotoSerifSC-Regular.ttf", "NotoSerifSC-Regular.subset.ttf"),
    ("Inter-Regular.ttf", "Inter-Regular.subset.ttf"),
    ("JetBrainsMono-Regular.ttf", "JetBrainsMono-Regular.subset.ttf"),
    # 真 700 字面（SIL OFL）：粗体不再用「字号 +0.8」冒充。原始文件不入库，
    # 由 Google Fonts 的可变字体实例化（wght=700）后本脚本裁子集。
    ("NotoSansSC-Bold.ttf", "NotoSansSC-Bold.subset.ttf"),
    ("NotoSerifSC-Bold.ttf", "NotoSerifSC-Bold.subset.ttf"),
    ("Inter-Bold.ttf", "Inter-Bold.subset.ttf"),
    ("JetBrainsMono-Bold.ttf", "JetBrainsMono-Bold.subset.ttf"),
    # Carlito：度量与 Calibri 兼容（SIL OFL）。只用于排版宽度计算，不含 CJK 字形。
    ("Carlito-Regular.ttf", "Carlito-Regular.subset.ttf"),
]

MEDIUM_DST = "NotoSansSC-Medium.subset.ttf"


def ui_charset() -> set[str]:
    """GB2312 全字符 + ASCII + 常用标点符号 + 源码字符串字面量字符（用于 UI 500 Medium 子集）。"""
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
    # 不换行空格：Excel 常用 \u00a0 做对齐，字体缺了会显示成问号
    chars.update("\u00a0\u202f\u2007\u2009\u200a")
    chars.update("①②③④⑤⑥⑦⑧⑨⑩ⅠⅡⅢⅣⅤ")
    # 三角形与常用 UI 符号（功能区下拉箭头用 ▾，缺字形会显示成方框）
    chars.update("▾▼▸◀▶◁▷●○◆◇■□▲△✓✕⚠ℹ•")
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
    return chars


def charset() -> str:
    chars = ui_charset()
    # 简繁词典里的字：转换结果（軟/隨/兌…）不在 GB2312 里，缺字形就会渲染成方框。
    blob_path = os.path.join(HERE, "crates", "core", "assets", "zhconv.bin")
    if os.path.exists(blob_path):
        b = open(blob_path, "rb").read()
        if len(b) > 8 and b[:4] == b"ZHC1":
            n = int.from_bytes(b[4:8], "little")
            at = 8
            for _ in range(n):
                if at + 2 > len(b):
                    break
                kl = int.from_bytes(b[at:at + 2], "little")
                at += 2
                k = b[at:at + kl].decode("utf-8", "ignore")
                at += kl
                if at + 2 > len(b):
                    break
                vl = int.from_bytes(b[at:at + 2], "little")
                at += 2
                v = b[at:at + vl].decode("utf-8", "ignore")
                at += vl
                chars.update(k)
                chars.update(v)
    return "".join(sorted(chars))


def build_medium_subset() -> bool:
    """实例化真 500 Medium 字面（SIL OFL 1.1 Noto Sans SC）并按 GB2312 + UI 字符串裁子集。"""
    static_src = os.path.join(SRC, "NotoSansSC-Medium.ttf")
    vf_candidates = [
        os.path.join(SRC, "NotoSansSC-VF.ttf"),
        os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts", "NotoSansSC-VF.ttf"),
    ]
    ui_text = "".join(sorted(ui_charset()))
    ui_text_file = os.path.join(OUT, "charset-ui-medium.txt")
    with open(ui_text_file, "w", encoding="utf-8") as f:
        f.write(ui_text)
    print(f"UI Medium 字符集 {len(ui_text)} 个字符")

    tmp_inst = None
    src_path = static_src if os.path.exists(static_src) else None
    if src_path is None:
        vf_path = next((p for p in vf_candidates if os.path.exists(p)), None)
        if vf_path is None:
            print("跳过（缺 NotoSansSC-Medium.ttf 或 NotoSansSC-VF.ttf）")
            return False
        from fontTools.ttLib import TTFont
        from fontTools.varLib import instancer

        vf = TTFont(vf_path)
        inst = instancer.instantiateVariableFont(vf, {"wght": 500}, inplace=False)
        inst["OS/2"].usWeightClass = 500
        fd, tmp_inst = tempfile.mkstemp(prefix="noto_sc_medium_", suffix=".ttf")
        os.close(fd)
        inst.save(tmp_inst)
        vf.close()
        inst.close()
        src_path = tmp_inst

    dst_path = os.path.join(OUT, MEDIUM_DST)
    try:
        cmd = [
            sys.executable, "-m", "fontTools.subset", src_path,
            f"--text-file={ui_text_file}",
            f"--output-file={dst_path}",
            "--layout-features=*",
            "--name-IDs=0,1,2,3,4,5,6,13,14,16,17",
            "--no-hinting",
        ]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            print(f"失败 {MEDIUM_DST}: {r.stderr[-300:]}")
            return False
        print(f"NotoSansSC-Medium (wght=500) -> {MEDIUM_DST} {os.path.getsize(dst_path)//1024} KB")
        return True
    finally:
        if tmp_inst and os.path.exists(tmp_inst):
            os.remove(tmp_inst)


def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    if "--medium-only" in sys.argv:
        return 0 if build_medium_subset() else 1
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
    build_medium_subset()
    return 0


if __name__ == "__main__":
    sys.exit(main())
