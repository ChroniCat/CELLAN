# C16 / QA-09：整表默认行高 + 图片 anchor 偏移（2026-11-17）

真实导出对拍：同一份 `crates/core/tests/fixtures/c16/objects.xlsx` / `print.xlsx`，
本程序（`SC_EXPORT_PDF` 走导出对话框同一个 `export_pdf_to`）与 Excel 16.0、WPS 12.0 各自导出的 PDF。

## 缺陷 1：`sheetFormatPr defaultRowHeight` 从来没读

`objects.xlsx` 的 `<sheetFormatPr defaultRowHeight="14.25">`、`print.xlsx` 的是 `15`，
但模型里 `Axis` 的默认行高是常数 `DEFAULT_ROW_H = 18px`（13.5pt），加载器只读 `<row ht=...>`。

实测后果（objects.xlsx，PDF 里「月份」→「一月」的行距）：

| 导出 | 行距 |
|---|---:|
| 修复前 | 13.5 pt |
| WPS | 14.2 pt |
| Excel | 12.5 pt（Excel 导出链路整体缩小 ~0.9，见下） |
| 修复后 | **14.25 pt** |

改法：`merges::SheetLayout` 增 `row_default`，`sheetFormatPr@defaultRowHeight` → `pt_to_px`
（14.25pt→19px、15pt→20px），`xlsx::load` 里 `Axis::set_default` 后再落各行自定义高度。
注意 `load_layout` 原先只在「有合并/列宽/行高/条件格式…」时才把这张表放进 map，
只有 `row_default` 的表会被丢掉——补进判据。

## 缺陷 2：图片矩形按「含 to 格」累加，且完全忽略 anchor 偏移

旧 `print::grid_rect` 用 `c1..=c2` 逐格累加，等于把 to 列也算满；`Picture` 里也没存
`<xdr:colOff>/<xdr:rowOff>`。于是同一张 96×64 的 logo，三方导出：

| 导出 | 图片 bbox (pt) | 尺寸 |
|---|---|---:|
| 修复前 | (50.4, 391.5) – (212.4, 459.0) | 162.0 × 67.5 |
| 修复后 | (70.4, 414.0) – (166.4, 478.0) | **96.0 × 64.0** |
| WPS | (69.8, 413.6) – (165.8, 477.6) | 96.0 × 64.0 |
| Excel | (69.6, 369.2) – (161.8, 425.4) | 92.2 × 56.2 |

改法：

- `pic::Picture` 增 `off: [i64; 4]`（from dx,dy、to dx,dy，单位 EMU，1px = 9525 EMU），
  从 `<xdr:from>/<xdr:to>` 里解析；拖动/缩放落点把 `off` 清 0（写盘也写 0 偏移）。
- `print::grid_rect` 改成 `from/to` 边界格 + EMU 偏移，不再逐格累加。
- 屏幕绘制/选框/命中同一条 `pic_rect` 闭包，偏移按 zoom 缩放。

修复后本程序与 WPS 的 logo 矩形差 **0.6pt**（x 0.6、y 0.4、宽高 0），
差异来自两者页边距取整；Excel 那份差得多是它导出链路自己的缩放（下一节），不是 anchor。

## Excel 导出链路缩放（已知，不按它调布局）

`print.xlsx`：Excel 自己 `HPageBreaks` 在 **A49**（第 1 页 48 行），WPS 也是 48 行；
本程序 `paginate` 在 scale=1 时第 1 页 44 行（可打印高 697.9pt、标题行 51.75pt、
行高 20px=15pt）。Excel/WPS 的 PDF 里行距都是 **13.6pt**（15pt × ~0.9），
横向列位置也各缩各的（相对 A 列：Excel ×0.88、WPS ×0.96），
即两份 PDF 都是「导出时整体缩放」而不是布局分页差异。本程序不做这种导出缩放，
所以**不拿 Excel PDF 的绝对坐标当列宽/行高基准**；能对的是内容、页数、
重复标题、分页处的行号与 WPS（其导出缩放更接近 1）。

## 证据

`docs/excel-wps-audit-2026-11-12/implementation-evidence/qa09/pdf-objects-bbox.json`
（各导出第 1 页的图片 bbox 与开头 12 个词的坐标）。

## 回归

```
cargo test -q -p sheetcore --offline -j 2            → CORE_EXIT=0
cargo test -q -p app --bin app -- --test-threads=1   → APP_EXIT=0（378 passed / 1 ignored）
cargo test -p sheetcore --test c16_pic_anchor        → 2 passed
```

新增测试：

- `crates/core/tests/c16_pic_anchor.rs`：图片矩形按 to 边界算（不含 to 格）、
  EMU 偏移改变位置与宽高。
- `pic::tests::parses_picture_anchor`：`[152400, 127000, 609600, 133350]` 偏移逐项解析。
