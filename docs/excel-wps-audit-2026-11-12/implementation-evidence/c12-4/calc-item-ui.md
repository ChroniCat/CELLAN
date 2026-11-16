# C12.4 计算项界面入口（2026-11-17）

透视表对话框里加一行：

```
计算项  [名称=项1+项2（如 合计=华东+华北）]
```

生成的流程：

1. `parse_calc_item()` 解析「名称=项1+项2」（容忍空格、空段）；
2. `apply_pivot2()` 在 `compute()` 之后调用 `append_calc_item_row()`，把这几个来源行逐列相加
   成一行插到总计行之前；
3. 后续 `write_grid()`、`pivots_pending2`（保存时挂 pivotTable 部件）都基于这个**已经多一行的**
   grid，所以 `rowItems`/`location`/`sharedItems f=1`/`pivotField item f=1` 会自然一致——
   这正是之前二分时确认的必要条件。

来源项找不到时不清空表格，状态栏提示「计算项「X」没找到来源项：A+B」，网格按普通透视生成。

## 验证

- `parse_calc_item_reads_name_and_sources`：`合计=华东+华北`、带空格写法、缺名字/缺来源/纯空白 → None；
- app 全量 **365 passed**；
- 端到端 XML 行为由 `calc_item_row_reaches_row_items_and_cache`（core）与 Excel 实测
  `calcrow-ours.xlsx → range=A9:E14, CalculatedItems=1` 覆盖。

## 未验

本机无交互桌面，**没有真人在这行输入框里敲过计算项再点生成**；界面位置/宽度未经截图验收，
按既有口径登记「已接线、待真人点击」。
