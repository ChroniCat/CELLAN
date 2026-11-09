# 内嵌字体

全部为 SIL Open Font License 1.1，可随程序分发。

| 文件 | 用途 | 子集体积 |
|---|---|---:|
| subset/NotoSansSC-Regular.subset.ttf | 中文黑体（界面/标题） | 2.2 MB |
| subset/NotoSerifSC-Regular.subset.ttf | 中文宋体（正文） | 3.1 MB |
| subset/Inter-Regular.subset.ttf | 英文正文 | 59 KB |
| subset/JetBrainsMono-Regular.subset.ttf | 等宽（公式栏/数字） | 63 KB |

原始 ttf 不提交，用 `uv run --with fonttools python scripts/subset_fonts.py` 重新生成。
子集范围：GB2312 一级+二级全部汉字（7542 字符）+ ASCII + 常用符号。

许可证：OFL 1.1，见 https://scripts.sil.org/OFL
