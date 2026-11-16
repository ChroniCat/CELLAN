# C12.4 计算项（CalculatedItem）：XML 已能生成，Excel 仍拒绝打开（未完成）

## Excel 自己怎么写

`$pt.PivotFields('部门').CalculatedItems().Add('合计A','=研发+销售')` 存盘后：

```xml
<cacheField name="部门" ...><sharedItems count="4">…<s v="合计A" f="1"/></sharedItems></cacheField>
…
<calculatedItems count="1">
  <calculatedItem formula="部门[研发]+部门[销售]">
    <pivotArea cacheIndex="1" outline="0" fieldPosition="0">
      <references count="1"><reference field="1" count="1"><x v="3"/></reference></references>
    </pivotArea>
  </calculatedItem>
</calculatedItems>
```

透视表侧：该字段 `items` 里多一条 `<item f="1" x="3"/>`，`items@count` 加一。

## 我们的实现

`pivot_xml::add_calculated_item()` 按上面的形状改两处（缓存 sharedItems + calculatedItems 块、透视表 items），
用例 `calculated_item_writes_cache_items_and_calculated_items_block` 断言：

- `<s v="华东华北" f="1"/>` 存在；
- `<calculatedItem formula="地区[华东]+地区[华北]">` 存在；
- `calculatedItems` 排在 `extLst` 之前（没有 extLst 时在根结束标签前）；
- `<item f="1" x="2"/>` 写进对应 pivotField。

## 但 Excel 打不开这个文件

`Workbooks.Open` 后 `Worksheets.Count = 0`（等同打不开/被修复丢弃），而同一夹具**不加计算项**的版本
`compact-multi.xlsx` 探针正常（`pivots=1 rows=1 cols=1 data=1 range=A9:E13`）。所以是我们的写法和
Excel 的隐含要求还有差距，**不是夹具问题**。

已排除：包内 XML 全部可解析；`calculatedItems` 位置正确；两个 count 都已同步；
`<x v="2"/>` 自闭合（早先漏 `/>` 的错误已修，但修好仍打不开）。

未排除：`cacheIndex` / `field` 的取值口径（Excel 用 1 表示第二个缓存字段，我们用 0）；
`<pivotArea>` 是否还需要 `dataOnly`/`labelOnly` 等属性；`sharedItems` 里 `f` 项的 `s` 与 `x` 双写要求。

## 结论

计算项**未完成**：XML 形状对得上但 Excel 不认，按「未实现」记，不计入 C12.4 完成项。
下一步应先反查 Excel 真实修正点（例如把 cacheIndex/field 改成 1 复测，或对比我们与 Excel 文件的
`pivotArea` 全属性差异），再决定是否继续。
