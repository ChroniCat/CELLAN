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
