# 真实缺陷：pivotCacheDefinition 缺版本属性 → Excel 直接丢弃透视表（2026-11-17）

## 症状

`work/probe/c12/pivot-real.xlsx` 在 Excel 16.0 里 `Worksheets(1).PivotTables.Count = 0`，
同一份数据由 Excel 自己生成的 `c12-4/excel-made.xlsx` 是 1。文件能打开、不报错、不提示修复，
透视表就是"不存在"。

## 二分定位（拿 Excel 自己写的部件替换我们的部件）

| 变体 | 替换内容 | Excel 结果 |
|---|---|---|
| hybrid2 | 我们的 pivotTable + **Excel 的 cacheDefinition+Records** | pivots=1 |
| hybrid3 | 我们的 pivotTable + **Excel 的 cacheDefinition**（records 仍是我们的） | pivots=1 |

→ 问题锁定在 `xl/pivotCache/pivotCacheDefinition1.xml`，与 pivotTable1.xml、rels、Content_Types 无关。

## 根因

Excel 写的缓存根节点带三个属性，我们的没有：

```
createdVersion="8" refreshedVersion="8" minRefreshableVersion="3"
```

没有 `refreshedVersion` 时 Excel 认为缓存格式未知 → 整个 pivot 部件被忽略（不是报错，是静默丢弃）。

## 修复

`crates/core/src/pivot_parts.rs`、`crates/core/src/pivot_parts2.rs` 的缓存根节点补上三属性；
`pivot_real` 用例新增断言（缺任一属性即失败）。

## 验证

重造 `work/probe/c12/pivot-fixed.xlsx`（干净 Excel 实例 + 只读打开 + 1.5s 等待）：

```
wb=pivot-fixed.xlsx sheets=1
ws=表格
pivots=1
```

## 探针坑（记录避免误判）

同一 Excel 实例里连续 `Workbooks.Open` 多个含透视表的文件会出现
`MethodNotFound` / `$ws.Name` 为空 / `pivots=` 空白，看起来像"文件坏了"。
稳定做法：每个文件单独起 Excel 实例、`Open(path, 0, $true)` 只读、打开后等 1.5s。
早期 `pivot-real.xlsx pivots=0` 这个读数本身是真实的（缺版本），但复现时的多次读数不可信。
