# C12.4 显示值方式：实现与验证（2026-11-17）

## 实现

- `pivot_multi::ShowAs { Normal, PercentOfRow, PercentOfColumn, PercentOfTotal }`，
  落到 `ValueField.show_as`（33 处构造点全部补齐，app 的「+ 值」菜单用 Normal）。
- `pivot_multi::compute` 末尾 `apply_show_as()`：聚合完成后按行合计/列合计/全表合计换算**比值**，
  与 Excel 一致（0.6 而不是 60）；分母为 0 时写空。
- `pivot_parts2` 的 `<dataField>` 输出
  `showDataAs="percentOfRow|percentOfColumn|percentOfTotal" numFmtId="10"`。

## 单测（用本夹具实际数据）

夹具：地区(华东/华北) × 产品(A/B) × 金额（10/20/30/40/50/60）。行序按拼音，华北在前。

| 模式 | 华北/A | 期望 |
|---|---|---|
| PercentOfRow | 60/120 = 0.5 | ✅ |
| PercentOfColumn | 60/100 = 0.6 | ✅ |
| PercentOfTotal | 60/210 = 0.285714… | ✅ |

同时断言 XML 含 `showDataAs="percentOfTotal"` 与 `numFmtId="10"`。
`pivot_multi_parts` **9 passed**、app **361 passed**。

## Excel 侧回读

```
pivots=1
calc=8            ← Excel 把 showDataAs="percentOfTotal" 还原成 Calculation=8
fmt=0.00%         ← numFmtId=10 生效
```

即 Excel 认得我们写的显示值方式。`DataBodyRange.Cells(1,1)` 读到空值——因为透视表主体
由 Excel 自己的渲染决定，我们写的是值化网格，这一点和 C12.4 既有口径一致（粘贴为值 +
另挂 pivotTable 部件），不算缺陷，但也不宣称「Excel 重算后数值一致」。

## 未做

- 界面还没有「显示值方式」下拉（只有核心能力与 XML），登记未做；
- `baseField/baseItem`（基于某项的百分比、差异、累计）未做。
