# C12.4 布局开关（表格形式 / 压缩形式）端到端（2026-11-17）

## 问题

界面上早就有复选框 `spec.layout.tabular`（`crates/app/src/main.rs:22041`
「表格形式（每层标签都显示）」），核心的 `pivot_multi::compute` 也按它决定行标签是否重复；
但 `pivot_parts2` 生成 `pivotTableDefinition` 时把布局属性硬编码成大纲形式：

```
outline="1" outlineData="1" compact="0" compactData="0"
```

结果：界面取消勾选后，单元格按压缩形式排了，导出的 XML 仍是大纲形式，Excel 打开又按大纲重排——
典型的「能选但不生效」中间态。

## 修法

`pivot_parts2` 在生成定义后按 `spec.layout.tabular` 改写根节点四个属性：

```rust
if !spec.layout.tabular {
    table = table.replace(
        "outline=\"1\" outlineData=\"1\" compact=\"0\" compactData=\"0\"",
        "outline=\"0\" outlineData=\"0\" compact=\"1\" compactData=\"1\"",
    );
}
```

## 验证

用例 `pivot_multi_parts::compact_layout_reaches_the_xml_and_excel_reads_it`：
构造 `spec_b()`（行=地区、列=产品、两个值字段），把 `layout.tabular` 置 false，
断言 `pivotTable1.xml` 含 `compact="1" compactData="1"` 与 `outline="0" outlineData="0"`。
`pivot_multi_parts` **8 passed**。

Excel 稳定探针：

```
compact-multi.xlsx sheet=Data pivots=1 name=数据透视表1 rows=1 cols=1 data=1 range=A9:E13
```

Excel 正常识别（不重建、不报修复）。

## 备注

`data=1` 而 spec 有两个值字段——Excel 只报了 1 个数据字段，需要单独查（可能是本次夹具只挂了一个
dataField，或 Excel 的 DataFields 计数口径不同）。未在本条里下结论。
