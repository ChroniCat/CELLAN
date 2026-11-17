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
