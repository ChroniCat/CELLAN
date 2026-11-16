# C12.4 显示值方式：Excel 语义实测（2026-11-17）

夹具 `work/probe/c12/regen/row_col.xlsx`（行=部门、列=姓名、值=金额），
第一个数据格 = 研发/张三 = 300，该行合计 500，该列合计 300，全表合计 1050。

`DataFields(1).Calculation` 逐值试（每次 `RefreshTable()` + `Calculate()` 后读
`DataBodyRange.Cells(1,1).Value2`）：

| Calculation | 首格值 | 含义 |
|---|---|---|
| 1 | 300 | 无变化 |
| 2 | 空 | — |
| 3 | 1 | — |
| 4 | 空 | — |
| 5 | 300 | 无变化 |
| **6** | **0.6** | 300 / 行合计 500 → **percentOfRow** |
| **7** | **1** | 300 / 列合计 300 → **percentOfColumn** |
| **8** | **0.285714285714286** | 300 / 全表 1050 → **percentOfTotal** |

存盘后 XML（Calculation=8）：

```xml
<dataFields count="1"><dataField name="求和项:金额" fld="2"
  showDataAs="percentOfTotal" baseField="0" baseItem="0" numFmtId="10"/></dataFields>
```

即：
- `showDataAs="percentOfTotal"` 对应 Calculation=8，`numFmtId="10"`（0.00%）；
- 推测 `percentOfRow` / `percentOfColumn` 同理（未单独存盘核对属性名，落实现时要再验一次）。

## 实现口径（待做）

1. `PivotSpec2.values[i]` 增加 `show_as`（Normal / PercentOfRow / PercentOfColumn / PercentOfTotal）；
2. `pivot_multi::compute` 在算完聚合后按行/列/全表合计换算比值（**不是**再乘 100，Excel 存的是 0.6）；
3. `pivot_parts2` 的 `<dataField>` 写 `showDataAs` 与 `numFmtId="10"`；
4. 对拍：同一夹具用 Calculation=6/7/8 各存一份，与我们的输出逐格比对（0.6 / 1 / 0.285714285714286）。

## 探针注意

`DataFields(1).Calculation = 5` 看着像「占总计百分比」，实测**不改值**（仍是 300）；
真正的比例是 6/7/8。不要按枚举名猜，按实测值判断。
