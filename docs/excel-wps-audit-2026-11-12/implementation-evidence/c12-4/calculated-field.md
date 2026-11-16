# C12.4 计算字段（2026-11-17）

## Excel 的写法（COM 造一个算字段后读包）

`$pt.CalculatedFields().Add('总价','=金额*2')` 存盘后：

```xml
<cacheField name="总价" numFmtId="0" formula="金额*2" databaseField="0"/>
```

同时 `pivotFields` 的条数从 3 变成 4（多一个 `<pivotField compact="0" outline="0" showAll="0"/>`）。

## 实现

`pivot_xml::add_calculated_fields(path, &[(名字, 公式)])`：
只做包内字符串改写，不碰缓存记录与数据区：

1. `pivotCacheDefinition*.xml`：在 `</cacheFields>` 前追加
   `<cacheField name=".." numFmtId="0" formula=".." databaseField="0"/>`，并把 `cacheFields@count` 加一；
2. `pivotTables/pivotTable*.xml`：追加对应数量的 `<pivotField compact="0" outline="0" showAll="0"/>`，
   并把 `pivotFields@count` 加一。

## 踩到的坑

只改缓存、不改 `pivotFields` 时，Excel **根本打不开**这个文件，`Workbooks.Open` 抛
「不能取得类 Workbooks 的 Open 属性」。补齐 `pivotFields` 条数后同一个文件正常打开
（`pivots=1 rows=1 cols=1 data=1 range=A9:E13`）。这条记在这里，避免以后再把两个 count 拆开改。

## 验证

- 用例 `calculated_field_lands_in_cache_definition_like_excel`：断言 `formula="金额*2"`、
  `databaseField="0"`、`cacheFields@count` 与 `<cacheField name=` 条目数一致、返回 2 处改动；
  `pivot_multi_parts` 12 passed、`pivot_real` 5 passed。
- Excel COM 回读该文件：
  `calcfields=1 / name=总价 / formula==金额*2` —— Excel 认得这是计算字段。

## 未做

- 数据区不含算字段的列：Excel 打开后会按公式自己算（刷新时），我们写出的网格里没有这一列的值；
- 计算**项**（CalculatedItem，按轴上某一项算）未做，Excel 写在 `pivotField` 的 `<calculatedItems>` 里；
- 界面没有「计算字段」入口。
