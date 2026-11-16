# C12.4 显示值方式：界面下拉接线（2026-11-17）

值字段那一行现在是三个控件：字段下拉（82pt）、汇总方式下拉（64pt）、**显示值方式下拉（96pt）**：

```rust
egui::ComboBox::from_id_salt(format!("p2sa{i}"))
    .selected_text(spec.values[i].show_as.label())
    .width(96.0)
```

四个选项与 Excel 对应：

| 界面文案 | ShowAs | XML |
|---|---|---|
| 值 | Normal | （不写 showDataAs） |
| 占行合计% | PercentOfRow | showDataAs="percentOfRow" numFmtId="10" |
| 占列合计% | PercentOfColumn | showDataAs="percentOfColumn" numFmtId="10" |
| 占总计% | PercentOfTotal | showDataAs="percentOfTotal" numFmtId="10" |

## 测试

- `show_as_labels_and_xml_names_are_stable`：四个模式的 `label()` 与 `xml()` 全部断言，
  防止界面文案与 XML 名漂移；`pivot_multi_parts` **10 passed**。
- app 全量 **361 passed**（编译期已确认新控件接入对话框）。

## 未验

本会话无可交互桌面（`GetForegroundWindow()=0`），**没有截图证明下拉在真实窗口里的位置与宽度**；
与 UI-11.1 同类，登记为「已接线、待真人点击验收」，不写成已验收。
