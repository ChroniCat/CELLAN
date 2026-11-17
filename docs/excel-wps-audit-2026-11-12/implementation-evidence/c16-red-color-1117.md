# C16.1 更正：`[Red]` 负数颜色其实已经画进 PDF

台账自 2026-11-14 起一直写着「负数 `[Red]` 颜色未绘制（C16.2）」。
这次直接查 PDF 内容流，结论是**这条记录已过期**。

## 做法

夹具（openpyxl）：

```text
A1 = -987.65  format #,##0.00;[Red]-#,##0.00
A2 =  123.45  format #,##0.00;[Red]-#,##0.00
A3 = -1       format 0.00;[Blue]-0.00
```

导出：`cargo run -p sheetcore --example c16_pdf -- red-in.xlsx red.pdf`
（`print::export_pdf_from`，字体 assets/fonts/subset/NotoSansSC-Regular.subset.ttf）

然后解压 PDF 流，数颜色算子：

```text
red ops 1   blue ops 1   black ops 1
BT 1.000 0.000 0.000 rg /F1 11.00 Tf … Tm <…> Tj ET   ← A1 的负数用红色
```

三格各一种颜色、各出现一次，落在各自的文本绘制块里。

## 为什么本来就对

`display::view_at` 第 120 行：

```rust
let mut color = crate::style::format_color(&value, &st.format).unwrap_or(st.color);
```

`style::format_color` 按「正/负/零/文本」选段并在段内找 `[Red]`/`[ColorN]`；
`print` 的文本绘制直接用 `view.color`（print.rs:381），预览与 PDF 同源。
所以只有「没读过代码、凭观感记的台账」是错的，实现没有缺口。

## 台账更正

C16.1 的 not_done 里删掉这条；仍保留的差异是**逐页 Excel/WPS 视觉矩阵未做**（需人工或渲染器比对）。
