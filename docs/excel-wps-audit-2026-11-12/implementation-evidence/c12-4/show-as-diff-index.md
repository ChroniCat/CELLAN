# C12.4 显示值方式：差异 / 差异百分比 / 索引（2026-11-17）

## Excel 写出的 XML（同夹具，逐个 Calculation 存盘后读 `<dataField>`）

| Calculation | XML |
|---|---|
| 2 | `showDataAs="difference" baseField="0" baseItem="0"`（**没有** numFmtId） |
| 3 | `showDataAs="percent" baseField="0" baseItem="0" numFmtId="10"` |
| 9 | `showDataAs="index" baseField="0" baseItem="0"`（没有 numFmtId） |

也就是说：默认基准（上一项）时 Excel 一律写 `baseField="0" baseItem="0"`，
**只有百分比类才带 `numFmtId="10"`**——差值和索引不套百分比格式。

## 数值口径（Excel 实测，夹具 研发/张三=300、行合计 500、列合计 300、全表 1050）

- `difference` 首格空、第二格 −300（= 0 − 300，即与同一行里前一项的差）；
- `percent` 首格 1、后续 0；
- `index` 2.1 = 300 × 1050 ÷ (500 × 300)，即 **(值 × 总计) ÷ (行合计 × 列合计)**。

## 实现

`ShowAs` 增加 `DifferencePrev` / `PercentDifferencePrev` / `Index`：

- 三者都在同一行内按列顺序处理，**第一列留空**（没有上一项）；
- `DifferencePrev` = v − prev；`PercentDifferencePrev` = (v − prev) / |prev|；
- `Index` = v × 总计 ÷ (行合计 × 列合计)；
- 分母为 0 统一留空（`ratio()`），与 Excel 的空单元格一致；
- `percent_format()` 决定是否写 `numFmtId="10"`，`pivot_parts2` 据此输出。

界面下拉同步加了三个选项（与上一项差 / 与上一项差% / 索引）。

## 验证

`show_as_difference_percent_diff_and_index_match_excel`：本夹具（华北 A=60/B=60，行合计 120、
A 列合计 100、全表 210）逐格断言 —— 差：首列空、第二列 0；差%：首列空、0；索引：1.05
（= 60×210÷(120×100)），与 Excel 同一公式口径一致；同时断言索引的 XML **不带** numFmtId。

`pivot_multi_parts` **11 passed**、app **364 passed**。

## 未做

`baseField/baseItem` 取非默认基准（「基于某一项」「基于列/行」）未做：Excel 用字段序号 + 项序号
表达基准，需要把轴上的项编号一并写进定义，是另一套模型。
