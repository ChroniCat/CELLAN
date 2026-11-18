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

仍未做：非整数/未采样字号（如 13、15）只是插值；粗体/缩进在非 11 号只按
`size/11` 比例缩放，未逐项实测；真实鼠标双击列边界仍缺。不能把这三档
端到端值扩大成“所有字号、所有字体已精确”。
