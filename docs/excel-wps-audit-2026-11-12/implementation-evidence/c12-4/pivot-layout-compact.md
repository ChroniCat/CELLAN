# C12.4 透视表布局：compact / outline 切换（2026-11-17）

## 实现

`sheetcore::pivot_xml::set_pivot_layout(path, compact: bool) -> Result<usize>`：
重写 `xl/pivotTables/pivotTable*.xml` 根节点四个属性，zip 其余条目原样保留。

```
compact="1" compactData="1" outline="0" outlineData="0"   // 压缩形式
compact="0" compactData="0" outline="1" outlineData="1"   // 大纲形式（生成默认）
```

`pivot_real::pivot_layout_switches_between_compact_and_outline` 用例：
默认 `compact="0"` → 切压缩后 `compact="1"/compactData="1"/outline="0"` → 切回大纲恢复，
每一步都确认包内 `pivotCacheDefinition1.xml` 仍在。**4 passed**。

## Excel 验证（稳定探针 `scripts/pivot_excel_probe.ps1`）

```
compact-ours.xlsx sheet=表格 pivots=1 name=数据透视表1 rows=1 cols=1 data=1 range=A9:G14
```

与大纲形式同一 `TableRange1`（A9:G14），说明只改布局标记不影响区域与字段绑定。

## 仍未验的部分（不夸口）

Excel 的「压缩形式」除了这四个属性，还会改变**单元格排布**（行字段合并到一列做缩进、小计行位置）。
我们只切了属性，`pivot::write_to` 的写格逻辑没变，所以：

- 属性层：已验（Excel 接受、字段/区域一致）；
- 视觉层：**未验**。下一步用 `$pt.RowAxisLayout(1)`（Excel 自己把大纲转压缩）生成本地基准，
  再与我们 compact 文件的 `UsedRange` 单元格逐个比对，才算视觉等价。

## 探针备注

`$wb.PivotCaches().Create(...)` 与 `$ws.PivotTableWizard(...)` 在本机 Excel 16.0 + PowerShell 下
都报错（`PSMethod 没有 Create` / `COMException`），所以没有走「Excel 新建透视表」这条路；
改用 `RowAxisLayout` 做基准是可行的替代。

## 视觉层对拍（同日补）：14×7 区域只有 2 格表头文字不同

做法：`scripts/c12_layout_compare.ps1`

1. 打开我们的 `compact-ours.xlsx`，让 Excel 自己 `RowAxisLayout(1)`（大纲→压缩），另存为 `ref-compact.xlsx`；
2. 分别导出两个文件 `UsedRange.Text` 到 TSV（14 行 × 7 列）；
3. 逐行 diff。

结果：

```
L9 : ours = 求和项:金额 | 列标签 | ...      ref = 求和项:金额 | 姓名 | ...
L10: ours = 行标签 | 张三 | 李四 | ...      ref = 部门 | 张三 | 李四 | ...
diff lines = 2 / 14
```

- 数据格（研发 300/200/500 等）、总计行、行列位置**逐格相同**；
- 只有两个"角标"不同：我们显示 Excel 的兜底文案「行标签 / 列标签」，Excel 显示字段名「部门 / 姓名」。

两个文件的 `pivotTableDefinition` 根属性、`pivotFields` 列表（含 axis/compact/outline/showAll）
在文本层面完全一致，也都没有 `rowHeaderCaption`/`colHeaderCaption`。所以差异不在这些属性上，
而在 pivotField → cacheField 的名字映射：Excel 取不到名字时才退化成「行标签/列标签」。

### 结论

- 布局层（区域、缩进、小计/总计、压缩形式）：**12/14 行逐字节一致**，2 处差异是表头文案；
- 表头文案是**真实小缺陷**（Excel 显示字段名），未修；下一步对比两文件的 `<items>` 与
  `sharedItems` 映射，定位名字丢失点。

产物：`work/probe/c12/layout/ours.tsv`、`ref.tsv`、`ref-compact.xlsx`、`compact-ours.xlsx`。

## 定位到根因：Excel 打开时把我们整张透视表重建了（cacheId 变了）

对比 `compact-ours.xlsx` 与 Excel `RowAxisLayout(1)` 之后的 `ref-compact.xlsx` 的
`pivotTable1.xml`：

| 项 | ours | ref |
|---|---|---|
| `cacheId` | **1** | **5** |
| `location ref` | A9:F13（5 行 6 列） | A9:G14（6 行 7 列） |
| `colItems count` | 5 | 6（多 `<i t="grand">`） |
| `rowItems` | `<x v="0"/>` → `<x v="2"/>` | `<x/>` → `<x v="1"/>` |
| `updatedVersion` | 6 | 8 |

`cacheId` 从 1 变成 5 说明 Excel **重新建了一个 pivot cache 并重建整张透视表**——
它不是「渲染我们的定义」，而是「按缓存重算一遍」。所以：

- 12/14 行相同，是因为两边都从同一份 cache 记录算出来的，不是我们定义被采纳；
- 表头显示「部门/姓名」而不是「行标签/列标签」，也是重建后的结果；
- 真正的缺陷在我们这一侧：**有列字段时，写出的 `location`/`firstDataRow` 少算了一行
  列字段表头，`colItems` 也少了总计项**（ours 5 条 vs ref 6 条）。定义与单元格不一致，
  Excel 只能重建。

## 结论与下一步

- 缺陷性质：`pivot_xml::attach` 在 `col_field.is_some()` 时按「单行表头」生成 `location`
  和 `firstDataRow`，而 `pivot::write_to` 写的是两行表头（列字段名 + 值字段名）。
- 下一步修：按 `has_col_field` 计算 `firstHeaderRow=2`/`firstDataRow=3`，`location` 行数 +1，
  `colItems` 补 `<i t="grand">`，再跑同一套 `c12_layout_compare.ps1` 验证 `cacheId` 保持为 1
  （不重建）且 TSV 14/14 全同。
- 在修好之前，本条**不算验收**：现在只是"Excel 能用缓存重建出正确结果"。

## 几何实测（决定修法前必须先量）

`pivot_real::dump_geometry_for_row_col` 打印真实结果：

```
[geom] row_only headers=["部门","金额"]                                   rows=4 rect=(20,0,24,1) → A21:B25
[geom] row_col  headers=["部门","张三","李四","王五","赵六","钱七"]        rows=4 rect=(20,0,24,5) → A21:F25
```

对照 `ours.tsv` 里 Excel 渲染出的形状（7 列、2 行表头）：

| 项 | 我们写出 | Excel 期望 |
|---|---|---|
| 表头 | 1 行（6 格） | 2 行（列字段名行 + 行字段名/值名行） |
| 合计列 | 无 | 有（第 7 列「总计」） |
| 合计行 | 有（第 5 行） | 有 |

所以缺陷不止 XML 属性：`pivot::compute` 在有列字段时**根本没生成总计列和列字段表头行**，
`write_to` 自然也就少写；XML 只是如实地把「少一列一行」记录下来，Excel 打开发现不一致 → 重建。

## 修法（三处必须同批改，否则定义与单元格继续错位）

1. `pivot::compute`：`col_field.is_some()` 时 headers 增加「总计」列，并返回两行表头模型
   （列字段名行 + 现有 headers）；
2. `pivot::write_to`：写两行表头，数据从第三行起，rect 相应 +1 行 +1 列；
3. `pivot_parts`：`firstHeaderRow="2"`、`firstDataRow="3"`、`location` +1 行、`colItems` 补
   `<i t="grand"><x/></i>` 且 `col_count` +1。

验收判据（写在台账里）：`c12_layout_compare.ps1` 重跑后 `cacheId` 保持 1（Excel 不重建）+ TSV 14/14 全同。
