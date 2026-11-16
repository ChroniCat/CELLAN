# C12.2/C12.3 对拍：页筛选（2026-11-17）

夹具（地区/产品/金额/状态，6 行），Excel 把「状态」放到页筛选并选「有效」。

## Excel 真值（`filter-ref.tsv`）

```
状态  有效
(空)
地区  求和项:金额
华北  120
华东  10
总计  130
```

数值口径：华北 = 20+40+60 = 120，华东 = 只剩「有效」的 10（A30 被筛掉），总计 130。

## 我们

`FilterField { col: 3, keep: ["有效"] }` → 网格行：

```
华北  120
华东  10
总计  130
```

数值、行顺序、总计**完全一致**（用例 `page_filter_matches_excel_filter_ref`，`pivot_multi_parts` 21 passed）。

## 差异（1 条）

Excel 在透视表上方渲染**页筛选标题区**（「状态 | 有效」两行），我们的网格没有这两行。
这部分属于「页筛选可视化」，不是筛选语义；C12.3 若要求显示页筛选标题，需要单独实现。

**筛选语义已对上；页筛选标题区未实现，C12.3 不计入验收。**

## 页筛选标题区已实现（同日）

`PivotGrid.page_filters: Vec<(字段名, 选中值)>`：

- `compute()` 从 `spec.filters` 里 `keep` 非空的项生成；
- `write_grid()` 在表头之前写「字段名 | 值」各一行，再空一行（与 Excel 一致），`rect` 从标题区起算；
- 断言：`page_filter_matches_excel_filter_ref` 增加 `page_filters == [("状态","有效")]`、
  `(2,1)=状态`、`(2,2)=有效`、第 3 行空、第 4 行是表头「地区」、`rect.0 == 2`。

连带修正两处写死行号/高度的用例：

- `write_grid_lands_values_at_the_target_anchor`：按内容找表头行；`rect.2` 期望改为
  `3 + 标题行 + 空行 + headers + rows - 1`；
- app `multi_field_pivot_refresh_clears_old_cells`：清筛选后标题区消失、明细多一行，
  断言改为「高度变化可解释」而不是「不变小」。

结果：`pivot_multi_parts` **20 passed + 1 ignored**、`c12_multi` **5 passed**、app **366 passed**。
