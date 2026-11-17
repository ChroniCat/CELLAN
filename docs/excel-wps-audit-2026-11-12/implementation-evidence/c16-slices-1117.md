# C16 切片复核（2026-11-17，自检，无监督）

依据：`core-requirements.md` C16 条目 + 原始验收“打印和 PDF 必须反映工作表真实显示”。
所有结论都有可复跑命令；Excel 侧一律以本机 Excel 16.0 的 `ExportAsFixedFormat` 产物为基准。

## C16.1 数字格式与字体显示模型 —— 已验收

| 检查 | 命令/产物 | 结果 |
| --- | --- | --- |
| 数字格式（含单段负数 `[Red]`） | `cargo test -p sheetcore --test pdf_export` / `--test c16_print::display_model_resolves_alignment_formats_and_colors` | 通过 |
| `--987.65` 渲染 | `c16-red-color-1117.md`（PDF 流三色各 1 次，落在各自 `Tj` 前） | 通过 |
| 字体/粗斜体/颜色进 PDF | `c16_print.rs` 3 + `c16-red-color-1117.md` | 通过 |
| 排版宽度用 Calibri 兼容度量 | 内嵌 Carlito（SIL OFL）`display::char_width` | 通过 |

## C16.2 合并换行对齐 / 分页重复标题 —— 待验收（只剩 1 字断行差）

已满足并有用例锁住：

- `title_rows_repeat_on_every_page_and_body_never_repeats`：每页都有表头「产品/金额」，
  且「行NN」集合两两不相交（正文不重不漏，覆盖 20+ 行）；
- `body_cells_are_not_duplicated_in_continuation_pages`、`csv_of_pages_matches_expected_row_split`；
- 合并格自动换行与预览共用 `display::wrap_lines_with`；`every_op_stays_inside_the_page`
  断言所有文字/矩形不越出纸张。

未满足：A8 自动换行的**断行点差 1 个字**（Excel 断在「…模型折｜行」，我们「…模型折行｜，」）。
根因见 `c16-render-compare-1117.md`：Excel 导出 PDF 时整页按 ~0.915 缩放并在缩放后的
宽度里折行；我们按文档 `scale=100` 排版。缩进单位已按 Excel 口径改成「3 个 0 宽度」，
断行差从 2 字缩到 1 字。要完全一致必须复刻导出缩放系数，而该系数不是文档属性
（COM 显式设 `Zoom=100` 后导出不变）。

## C16.3 图片/图表/文本框 —— 已验收

- `page_ops_emit_picture_chart_and_textbox`：1 张 PNG（字节非空）、图表矩形/坐标轴线段、
  标题「季度销量」、文本框内容都在绘制指令里；
- `pdf_embeds_images_and_object_text`：图片与文字进 PDF；
- `value_axis_ticks_use_excel_nice_steps`：数值轴 0/5/10/15/20/25 与 Excel 完全一致，
  不再出现 6.25/12.5/18.75；标题只画一次（此前“多画标题/缺轴标签”两项均已消除）。

## C16.4 预览与 PDF 逐页一致 —— 已验收（代码同源 + 用例）

- 预览侧 `crates/app/src/main.rs:6991` 调 `print::page_ops_with`，导出调 `page_ops_metric`；
- `preview_ops_are_the_pdf_ops`：逐页断言两条路径的文字（内容/坐标/基线）、矩形、线段完全相同；
- 度量统一：两者都走 `display::char_width`（Carlito + CJK 1em），不存在“预览一套、PDF 一套”。

限制（不属本切片）：真实鼠标点“打印预览”仍需要交互桌面，环境 `GetForegroundWindow()=0`，
记录在总台账 blocker 里。

## 复跑

```bash
cargo test -p sheetcore --offline -j 2 --test c16_print --test c16_objects -- --test-threads=1
# c16_print 9 passed / c16_objects 4 passed
```
