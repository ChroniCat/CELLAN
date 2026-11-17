# 内嵌字体

全部为 SIL Open Font License 1.1，可随程序分发。

| 文件 | 用途 | 子集体积 |
|---|---|---:|
| subset/NotoSansSC-Regular.subset.ttf | 中文黑体（界面/标题） | 2.2 MB |
| subset/NotoSansSC-Bold.subset.ttf | 中文黑体 700（粗体） | 4.1 MB |
| subset/NotoSerifSC-Regular.subset.ttf | 中文宋体（正文） | 3.1 MB |
| subset/NotoSerifSC-Bold.subset.ttf | 中文宋体 700（粗体） | 5.7 MB |
| subset/Inter-Regular.subset.ttf | 英文正文 | 59 KB |
| subset/Inter-Bold.subset.ttf | 英文正文 700（粗体） | 103 KB |
| subset/JetBrainsMono-Regular.subset.ttf | 等宽（公式栏/数字） | 63 KB |
| subset/JetBrainsMono-Bold.subset.ttf | 等宽 700（粗体） | 69 KB |

子集范围：GB2312 一级+二级全部汉字（7542 字符）+ ASCII + 常用符号。

## 重新生成

常规字面用仓库里的原始 ttf：

```bash
uv run --with fonttools python scripts/subset_fonts.py
```

粗体字面（`assets/fonts/*-Bold.ttf`，不入库）由 Google Fonts 的可变字体实例化到
`wght=700` 后再裁子集，缺文件时脚本会打印「跳过（缺文件）」：

```bash
# 1) 取可变字体（Inter 同时把 opsz 固定到 14）
curl -L -o assets/fonts/NotoSansSC-Bold.ttf  "https://raw.githubusercontent.com/google/fonts/main/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf"
curl -L -o assets/fonts/NotoSerifSC-Bold.ttf "https://raw.githubusercontent.com/google/fonts/main/ofl/notoserifsc/NotoSerifSC%5Bwght%5D.ttf"
curl -L -o assets/fonts/Inter-Bold.ttf        "https://raw.githubusercontent.com/google/fonts/main/ofl/inter/Inter%5Bopsz,wght%5D.ttf"
curl -L -o assets/fonts/JetBrainsMono-Bold.ttf "https://raw.githubusercontent.com/google/fonts/main/ofl/jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf"

# 2) 实例化到 700（Inter 另固定 opsz=14）
uv run --with fonttools python - <<'PY'
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
for src, axes in [("NotoSansSC-Bold.ttf", {"wght": 700}),
                  ("NotoSerifSC-Bold.ttf", {"wght": 700}),
                  ("Inter-Bold.ttf", {"wght": 700, "opsz": 14}),
                  ("JetBrainsMono-Bold.ttf", {"wght": 700})]:
    f = TTFont("assets/fonts/" + src)
    inst = instantiateVariableFont(f, axes, inplace=False, updateFontNames=True)
    inst.save("assets/fonts/" + src)
PY

# 3) 裁子集
uv run --with fonttools python scripts/subset_fonts.py
```

## 用在哪

- 界面/网格：`*-Regular` 与 `*-Bold` 都注册成 egui 字族（`default-bold`、
  `cjk-sans-bold`、`cjk-serif-bold`、`latin-bold`、`mono-bold`）。系统没有真粗体字面时
  一律用内嵌 700 字面，不再把常规字面放大冒充。
- PDF 导出：常规 + 粗体两套都嵌进 PDF（`/F1`、`/F2`），粗体文字下发真 700 字形，
  不做描边模拟；字体流用 Flate 压缩，一页含两套字体的 PDF 约 5.7 MB。

许可证：OFL 1.1，见 https://scripts.sil.org/OFL
