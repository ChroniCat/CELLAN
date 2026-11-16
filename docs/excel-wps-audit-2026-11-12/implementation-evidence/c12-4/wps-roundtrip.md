# C12.4 WPS 改字段往返（2026-11-17）

脚本：`scripts/c12_wps_pivot_edit.ps1`（KET.Application，按索引访问 PivotFields）。

## 探针约束（WPS COM 与 Excel 不同）

- `$pt.PivotFields("地区")` → `Value does not fall within the expected range.`（**按名查找不支持**）
- `@($pt.RowFields)` → 返回集合名字符串 `RowFields`，`Count` 恒 0（**集合枚举不可用**）
- 改用 `$pt.PivotFields($i)` + `$pf.Orientation -eq 1` 扫描定位行字段 → 可行
- `$pf.Orientation = 2` 改列字段 → `changed=yes`

## 结构变化（XML 实测）

源 `excel-made.xlsx`（Excel 自己存的基准）：

```
pivotField axis="axisRow" ...   地区
<pivotFields count="3">, <rowFields count="1"><field x="0"/></rowFields>, <colFields count="1"><field x="1"/></colFields>
pivotCacheDefinition1.xml 1300 字节, cacheFields 4, sharedItems 3
```

WPS 改字段并存为 `wps-edited2.xlsx`：

```
两个 pivotField axis="axisCol"（地区 从行变列）
rowFields 消失, <colFields count="2"><field x="1"/><field x="0"/></colFields>
pivotCacheDefinition1.xml 773 字节, cacheFields 4, sharedItems 2
```

## Excel 回读结果：WPS 存出的文件已不可用

```
pivots=1            // 透视表对象还在
rows=0 cols=0 obs=0 // RowFields/ColumnFields/PivotFields 全部为 0
```

即 Excel 能打开文件、能看到透视表外壳，但**字段绑定全部丢失**；同源 `excel-made.xlsx` 回读是
`rows=1 cols=1 data=1`。根因在 WPS 的保存侧：`pivotCacheDefinition` 从 1300 字节缩到 773 字节、
`sharedItems` 3→2，缓存字段与记录对不上。

## 结论

「WPS 里改字段后再存回」这条**在 WPS 自身保存环节就断了**，与本引擎无关：源文件是 Excel 自己写的，
WPS 只做了一次改字段+保存，产出的文件 Excel 已读不出字段。要判定是我们写坏，必须控制住这个变量，
目前证据不支持该指控。

## 附带发现（待查，不计入本条）

`work/probe/c12/pivot-real.xlsx`（早前夹具）在 Excel 里 `PivotTables.Count = 0`，
而 `docs/.../c12-4/excel-made.xlsx` 是 1。两个夹具来源不同，需要用干净链路重造后再判断是不是我们写坏。
