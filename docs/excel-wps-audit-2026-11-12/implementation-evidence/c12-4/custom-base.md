# C12.4 自定义基准（基准字段 / 基准项）

## 依据（Excel COM 真值）

`scripts/c12_base_field_ref.ps1` 显式设 `BaseField='产品'`、`BaseItem='A'`，Excel 写：

```xml
<dataField name="求和项:金额" fld="2" showDataAs="difference" baseField="1" baseItem="0"/>
```

`baseField` = 该字段在 `pivotFields` 里的序号（地区 0 / 产品 1 / 金额 2），
`baseItem` = 该项在**该字段 sharedItems** 里的下标（A=0、B=1）。

## 实现

- `pivot_multi::ValueField` 新增 `base: Option<(u32 /*源列*/, String /*项标签*/)>`；
  旧调用点统一 `base: None`（等于默认「上一项」）。
- `pivot_parts2::build` 写 `dataField` 时：
  `baseField = base_col - c1`，`baseItem = shared[base_col-c1].position(item)`（找不到按 0）；
  `None` 时保持 `baseField="0" baseItem="0"` —— 与 Excel 未指定基准时的输出逐字节一致。
- UI（多字段透视对话框，值字段那一行）：显示值方式选「与上一项差 / 百分比差」后，
  出现「基准」下拉（上一项默认 / 各轴字段）与「基准项」下拉（该列去重值）。

## 测试

`custom_base_field_and_item_written_like_excel`（`crates/core/tests/pivot_multi_parts.rs`）：

- `base: Some((1, "A"))` → 含 `showDataAs="difference" baseField="1" baseItem="0"`；
- `base: None` → 含 `baseField="0" baseItem="0"`。
