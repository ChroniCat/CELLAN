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
