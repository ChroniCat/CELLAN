# UI-14.3 列宽口径修正（2026-11-17）

原 `not_done`：「列宽比 Excel 宽 1.78 字符（默认字体度量差异），末字符完整」。

## 根因（两条，独立）

1. **AutoFit 用屏幕字体的 galley 量宽度**：ASCII 走 egui 的 Inter，比 Calibri/Carlito 宽约 15%，
   33 字符的英文串量出 232.8px，Excel 是 196.3px → 存回去正好宽 1.5~1.8 字符。
2. **像素↔字符换算漏了 5px 内边距**：内部 px 是显示宽（字符数×8+5），写文件却用 `px/8`，
   每存一次就多 0.625 字符；读文件又用 `width×8`，两头一起把误差放大。

## 修正

- `crates/core/src/layout.rs`：`col_width_to_px(w) = w×8 + 5`，默认宽 8.38 → 72px（Excel 实测一致）。
- `crates/core/src/patch.rs`：写 `<col width>` 用 `(px−5)/8`；`xls_write` 同样减 5。
- `crates/app/src/main.rs`：AutoFit 不再用 galley，改用
  `sheetcore::display::text_width()`（Carlito advance，CJK 按 1em），
  `ColumnWidth = (text_px − 3)/7`，再换回内部 px `chars×8+5`。

## Excel 实测（Excel 16.0 Build 20326 / 96dpi，见 `ui14/autofit-widths.txt`）

手工设 9 种宽度（8.43 / 9 / 10 / 12 / 15 / 21.25 / 30 / 5 / 40），本程序 load→save 后
Excel 读回 **9 列全部原值**（8.38 / 9 / 10 / 12 / 15 / 21.25 / 30 / 5 / 40）。

AutoFit 同一文件：

| 列 | 内容 | Excel ColumnWidth | 修正前 | 修正后 |
|---|---|---:|---:|---:|
| A | 20 个汉字（宋体 11） | 41.5 | 37.5 | **41.5** |
| B | 33 字符英文（Calibri 11） | 27.63 | 29.13 | **27.63** |
| C | 16 位整数（General） | 10.63 | 17.13 | 16.5 |

A/B 两列与 Excel 完全一致。C 列的剩余差异不是宽度换算，而是 General 数字显示策略：
Excel 在默认列宽下把 1234567890123456 显示成 `1.23E+11` 并按 ~11 字符定宽；
本程序仍显示完整数字，所以 AutoFit 按 16 位定宽（16.5 字符）。已记入 UI-14.3 `not_done`。

## 回归

```
cargo test -q -p sheetcore --offline -j 2                → CORE_EXIT=0（115 个测试二进制）
cargo test -q -p app --bin app -- --test-threads=1      → APP_EXIT=0（378 passed / 1 ignored）
```

改动测试期望：`merges::parses_cols_and_rows` 16 字符 → 133px；`xls_layout` 同步；
`xls_write` 往返列宽 140px 保持不变。

## 2026-11-18 第二次逐字符对拍（本机 Excel 16.0）

上一版把 AutoFit 改成 `text_width/7` 后，A/B 对齐但 11 位数字/科学计数仍有
1–1.5 字符差。重做 46 行字符串的 Excel `AutoFit()` 标定。这里有一个容易
踩的前提：**Excel 的 ColumnWidth 以当前 Workbook 的 Normal 字体为单位**。

- 本机中文版 Excel 新建工作簿的 Normal 是 **等线 11**：
  `1`→1.88、11 位数字→12.13（与台账里 `1.23457E+14` 的 12.13/76.5pt 一致）；
- 把 Normal 换成 Calibri 11 再 AutoFit，同一字符串会变成 1.54/11.69。
  第一版标定误用了后者，不能代表本项目的默认环境。

以下以 **等线 11** 为准（`work/probe/excel_autofit_calib2.out`）。Excel 不是
按字形 advance 求和：ASCII 每字符 1、CJK 每字符 2，固定加 0.88；超过 9 个
字符后每字符再加 0.125 的像素量化。

| 文本 | Excel ColumnWidth（等线） | 旧实现 |
|---|---:|---:|
| `1` | 1.88 | 2.38 |
| `111111111` | 9.88 | 9.54 |
| `1111111111` | 11.00 | 10.49 |
| `11111111111` | 12.13 | 11.86（多 +5） |
| `1234.5678` | 9.88 | 9.21 |
| `1.23457E+14` | 12.13 | 11.27（差 0.42，char 数差 1.04） |
| `中文字` | 6.50 | — |
| `中文文字中文` | 12.38 | — |
| `Hello World` | 12.13 | — |

修正：

- `crates/core/src/display.rs::excel_autofit_chars` 按上述单位公式实现；
  纯 CJK 串另按实测曲线（1 个=2.75、2 个=4.63、3 个=6.50、4 个=8.38、
  5 个以上每个 +2）处理；
- `crates/app/src/main.rs::autofit_col` 用它，去掉重复的 `+5`，最小宽度从
  24px 降到 17px（单字符列），空列仍保持 72px；
- 新增 `crates/core/tests/autofit_width.rs`，把 31 个代表值固化成断言。

端到端（`SC_AUTOFIT=c:N` + openpyxl 读回，用当前 Normal=等线的 Excel 取值）：

| 列 | 文本 | app | Excel |
|---|---|---:|---:|
| A | `12345678901` | 12.13 | 12.13 |
| B | `1234.5678` | 9.88 | 9.88 |
| C | `0.1` | 3.88 | 3.88 |
| D | `1` | 1.88 | 1.88 |
| E | `1.23457E+14` | 12.13 | 12.13 |

`cargo test -p sheetcore --test autofit_width`：1 passed。

## 缩进与粗体（2026-11-18 第三次对拍）

Excel 实测（`work/probe/excel_autofit_indent2.out`，Normal=等线 11）：

| 单元格 | 样式 | Excel ColumnWidth | app |
|---|---|---:|---:|
| `1` | 普通 | 1.88 | 1.88 |
| `1` | 粗体 | 2.25 | 2.25 |
| `1` | 缩进 1 | 4.88 | 4.88 |
| `1` | 缩进 2 | 7.88 | 7.88 |
| `1` | 粗体 + 缩进 1 | 5.25 | 5.25 |
| `12345678901` | 缩进 1 | 15.50 | 15.13 |
| `12345678901` | 缩进 2 | 18.75 | 18.13 |

新增 `excel_autofit_chars_styled(s, bold, indent)`：缩进按 **3 字符/级**，
粗体 **+0.37**，不再用旧的 `indent×size×2` 像素估算；app 的 `autofit_col`
改为直接用它。长数字缩进仍有 0.37–0.62 的像素量化偏差，未冒充逐位精确。

仍未做：真实鼠标双击列边界（无交互桌面）；多行/合并列、非 11 号的字号
未逐项对拍（字号对 AutoFit 的影响本轮只记录，不实现）。

## 自动换行与合并格（2026-11-18 第四次对拍）

Excel 实测（`work/probe/excel_autofit_wrap3.out`，单元格 等线 11、wrap=True）：

| 单元格 | 内容 | Excel ColumnWidth | app |
|---|---|---:|---:|
| F1 | `1\n22` | 2.88 | 2.88 |
| D1 | `第一行\n第二行` | 6.50 | 6.50（最长行 3 汉字） |
| E1 | `中文文字中文` | 12.38 | 12.38 |
| H1:I1 | 合并 + 长中文 | 8.38（AutoFit 不动合并区） | 72px 默认，未加宽 |

实现：`display::excel_autofit_chars_wrapped(s, bold, indent, wrap)` 在 wrap=True 时取
`split('\n')` 的最长一行；`autofit_col` 跳过任何落在 `sh.merges` 里的格子，
避免合并区长文本把普通列撑宽。回归 `wrapped_autofit_uses_longest_line` 3 passed；
端到端 F 列 `SC_AUTOFIT=c:5` 保存回读 `F=2.88`。

仍未做：真实鼠标双击列边界；非 11 号字号的 AutoFit；该夹具里 10 位 ASCII 数字串
Excel 给 7.88、等线 11 公式给 11.0，说明 ASCII 比例字体的量化尚未完全反解，
不能把多行/合并的对齐扩大成“所有字符串已精确”。

## 非 11 号字号曲线（2026-11-18 第五次对拍）

Excel 实测：`work/probe/excel_autofit_size_matrix2.ps1` 对 17 个字符串
（`1`…12 个 `1`、`中`…5 个汉字）× 10 个字号（8/9/10/11/12/14/16/18/20/24）
逐项设 `Font.Name=等线`、`Font.Size=字号`、重置 `ColumnWidth=8.43` 后
`AutoFit()`，输出 `work/probe/excel_autofit_size_matrix2.out`。每档曲线已固化在
`crates/core/src/display.rs::AUTOFIT_SIZE_CURVES`。

端到端（`work/probe/cua_autofit_size.xlsx`，字体 等线；`SC_AUTOFIT=c:N` 后读回
保存文件的 `<col width>`）：

| 列 | 文本 | 字号 | app chars | app 存盘宽度 | Excel ColumnWidth |
|---|---|---:|---:|---:|---:|
| A | `中文文字中文` | 14 | 16.01 | 16.01 | 16.00（四舍五入到 0.01 档） |
| B | `1` | 8 | 1.63 | 1.63 | 1.63 |
| C | `123456789012` | 14 | 15.88 | 15.88 | 15.88 |

实现：`excel_autofit_chars_at_size` / `excel_autofit_chars_wrapped_at_size`；
11 号仍走原 `excel_autofit_chars_styled` 精确模型，其他字号按相邻实测档
线性插值；纯 CJK 与 ASCII/混合串使用两条独立曲线。`autofit_col` 传入
`st.size`（Excel 字号，不用屏幕 px）。

新增断言 `autofit_uses_excel_size_curves`、`wrapped_autofit_respects_size`；
`cargo test -q -p sheetcore --offline --test autofit_width -- --test-threads=2`
= **5 passed**；app 全量
`cargo test -q -p app --bin app --offline -j 2 -- --test-threads=1`
= **386 passed / 0 failed / 1 ignored / APP_EXIT=0**（`app-autofit-size.log`）。

仍未做：非整数/未采样字号（如 13.5）才做相邻档插值；粗体/缩进在非 11 号只按
`size/11` 比例缩放，未逐项实测；真实鼠标双击列边界已补在 `ui14-double-click-1118.txt`。
不能把这三档端到端值扩大成“所有字号、所有字体已精确”。

## 8–24 逐磅位曲线（2026-11-18 第六次对拍）

为消掉“13/15 等未采样字号只是插值”的问题，把采样扩到 8–24 **每个整数磅位**：

`work/probe/excel_autofit_size_matrix3.ps1` 对同一组 17 个字符串
（`1`…12 个 `1`、`中`…5 个汉字）× 17 个字号（8…24）逐项设
`Font.Name=等线`、`Font.Size=字号`、重置 `ColumnWidth=8.43` 后 `AutoFit()`，
输出 `work/probe/excel_autofit_size_matrix3.out`（289 行）。`AUTOFIT_SIZE_CURVES`
已换成 17 档逐磅位实测；只有非整数磅位（如 13.5）才在相邻整数档之间线性插值。

端到端（`work/probe/autofit_frac.xlsx`，等线；`SC_AUTOFIT=c:N` 后读回
`work/probe/autofit_frac_outN.xlsx` 的 `<col width>`）：

| 列 | 文本 | 字号 | app 存盘宽度 | Excel AutoFit |
|---|---|---:|---:|---:|
| A | `中文文字中` | 13 | 12.00 | 12.00 |
| B | `111111111111` | 15 | 18.50 | 18.50（文本单元格） |
| C | `1` | 13.5 | 2.32 | 2.315（13=2.25、14=2.38 插值） |
| D | `中文文字中` | 17 | 16.13 | 16.13 |

**更正（2026-11-18 第七次对拍）**：B 之前的 17.13 是对 **数值**单元格
（`Value2` 12 位数字被 Excel 显示成科学计数法）测的；xlsx 里作为文本的
`111111111111` 在 15 号是 **18.50**。AutoFit 必须按“文件里实际显示成什么”测，
不能把 COM 直接赋值的数字串当文本。

`autofit_uses_excel_size_curves` 新增 13/15/17/19/21/22/23 三组真值，并把插值断言
改成 13.5（2.315）。`cargo test -q -p sheetcore --offline --test autofit_width --
--test-threads=2` = **5 passed**。

## 文本单元格与样式矩阵（2026-11-18 第七次对拍）

第六次的 COM 矩阵有个口径错误：`Value2="111111111111"` 会被 Excel 当 **数值**，
General 显示成 `1.11111E+11`，AutoFit 只有 12.38/17.13；而 xlsx 里作为**文本**的
同一串在 11/15 号是 **13.25/18.50**。因此重做了两个 **文件级** 矩阵（openpyxl 写成
`t="inlineStr"`，Excel 打开后逐列重置 `ColumnWidth=8.43` 再 `AutoFit()`）：

- `text_ascii_matrix.xlsx`：`1`…12 个 `1` × 字号 8…24，204 列；
  输出 `excel_text_ascii_matrix.out`（204 行）。`AUTOFIT_SIZE_CURVES` 的 ASCII 曲线
  已全部换成此文本口径；CJK 曲线仍用 `excel_autofit_size_matrix3.out`（汉字不会被数值化）。
- `style_file_matrix.xlsx`：3 个串（`1`、`中文文字中`、`111111111111`）× 字号 8…24
  × 5 样式（regular/bold/indent1/indent2/bold_indent1），255 列；输出
  `excel_style_file_matrix.out`（255 行）。`ELINE_STYLE_DELTAS` 用这份数据，
  粗体按 0/0.125/… 的像素量化，缩进按文件语义 3 字符/级（COM 直接设
  `IndentLevel` 的 1.5 不适用于打开文件，已作为反例记录）。

11 号文件级真值：

| 串 | regular | bold | indent1 | indent2 |
|---|---:|---:|---:|---:|
| `1` | 1.88 | 1.88 | 4.88 | 7.88 |
| `中文文字中` | 10.38 | 10.38 | 13.63 | 17.00 |
| `111111111111` | 13.25 | 13.25 | 16.63 | 19.88 |

端到端（`autofit_style_eline.xlsx`，五列分别 1/等线/indent1、1/等线/bold、
CJK5/等线/indent2、1/宋体/indent1、1/等线13/indent1；`SC_AUTOFIT=c:0..4`）：

| 列 | app 存盘 | Excel AutoFit |
|---|---:|---:|
| A `1` 等线 indent1 | 4.88 | 4.88 |
| B `1` 等线 bold | 1.88 | 1.88 |
| C `中文文字中` 等线 indent2 | 16.99 | 17.00 |
| D `1` 宋体 indent1 | 4.88 | 4.88 |
| E `1` 等线13 indent1 | 5.25 | 5.25 |

`autofit_width.rs` 新增 `eline_styled_autofit_matches_measured_deltas`（10 个文件级
真值），目标测试 **6 passed**。C 列 16.99 vs 17.00 是存盘把 141px 折回字符宽时的
0.01 舍入；像素宽度同为 141px。

## PDF/分页行覆盖（2026-11-18 第八次对拍）

UI-14 原始验收要求“导出 PDF 与屏幕行数一致”。PDF 导出和打印预览消费同一个
`print::paginate` 的 `Layout.pages`，新增 `crates/core/tests/print_pagination.rs`：

- 120 行正文 + 顶端标题行（标题不重复进正文），`print_area=(0,0,120,0)`；
- 断言分页后正文 1..120 每行**恰好出现一次**，无漏行/重复；
- 对每页调用 `print::page_ops_with`，断言每行文本在该页恰好生成一次；
- `cargo test -p sheetcore --test print_pagination` = **1 passed**。

PDF 的绝对坐标与 Excel 导出 PDF 有约 0.9x 的导出链路缩放（Excel 自身导出属性，
非版面模型差异）；同一 `Layout` 的行集合和行顺序与屏幕一致。
