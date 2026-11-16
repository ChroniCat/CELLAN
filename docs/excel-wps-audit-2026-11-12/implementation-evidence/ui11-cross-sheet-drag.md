# UI-11：工式编辑中跨表拖动扩区域（2026-11-17）

## 原来的断点

`insert_pick_reference` 已经会把跨表的单格写成 `Sheet2!B3`（UI-11.6），但拖动扩区域那条分支
用的是 `range_label(a, (r, c))`，**不带表名**，于是跨表拖出来是 `B3:C4`——没有表名就指回当前表，
公式错位。

## 改了两处

1. `switch_sheet_for_ref`（点别的表页接着取引用）在切表后清掉 `pick_anchor` / `pick_rect`：
   引用区间不能横跨两张表，Excel 也是把新表上的第一次点选当作新起点；
2. 拖动扩区域改用 `pick_range_label(sheet, a, b)`：
   - 同表：`B2:C4`
   - 跨表：`Sheet2!B2:C4`
   - 表名含空格：`'销售 数据'!A1`（Excel 的加引号规则）
   - 单格不写 `A1:A1`，只写 `A1`

## 验证

`cross_sheet_drag_labels_carry_sheet_name` 覆盖四条：同表、跨表、带空格表名、单格。
app 全量 **364 passed**。

## 未验

本机无交互桌面：**没有真人按住鼠标、点表页、再拖出区域**。这条仍按既有口径登记
「已接线、待真人点击」，不写成已验收。
