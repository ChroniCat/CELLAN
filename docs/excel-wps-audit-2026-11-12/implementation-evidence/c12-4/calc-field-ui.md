# C12.4 计算字段界面入口（2026-11-17）

透视表对话框里加一行：

```
计算字段  [名称=公式（如 总价=金额*2）]
```

## 链路

1. `parse_calc_field()`：`名称=公式`，名字/公式任一为空返回 None（公式里允许空格）；
2. `apply_pivot2()` 把 `(工作表, 名称, 公式)` 记进 `Workbook::pivot_calc_fields`（同名先删再加，
   避免反复点生成堆出重复条目）；
3. **保存时**由 `xlsx::save` 在挂完透视表部件之后调用
   `pivot_xml::add_calculated_fields()`，写进 `pivotCacheDefinition`：
   `<cacheField name="总价" numFmtId="0" formula="金额*2" databaseField="0"/>`，
   并把 `cacheFields@count` 与透视表 `pivotFields@count` 一起加一（上一轮验证过：两个 count
   不同步时 Excel 直接打不开文件）。

## 验证

- `parse_calc_field_reads_name_and_formula`：正常写法、带空格、缺名字/缺公式 → None；
- `calc_field_survives_save_and_reload`（core）：走真实 `xlsx::save` 后重新解包，
  断言 `formula="金额*2"`、`databaseField="0"`、`cacheFields@count` 与条目数一致；
- `pivot_multi_parts` **16 passed**、app **366 passed**。

## 已知限制

`add_calculated_fields` 会改所有 `pivotCacheDefinition` 部件；一个文件里有多张透视表时，
计算字段会被写进每一个缓存。当前模型（每张表挂一张透视表）下无影响，多透视表场景未支持。

## 未验

无交互桌面：**没有真人在输入框里敲过计算字段再保存**，界面位置/宽度未截图验收，
按既有口径登记「已接线、待真人点击」。
