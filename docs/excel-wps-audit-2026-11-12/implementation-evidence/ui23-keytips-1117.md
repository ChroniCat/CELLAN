# UI-23.1：完整 KeyTips（Excel UIA 实测 + 两级序列）

## Excel 真实 AccessKey 来源

用 UIA 客户端连 Excel 16.0 窗口，逐个 `TabItem.Select()` 后枚举所有带
`AccessKey` 的元素，得到 601 行原始数据（`work/probe/excel_uia_tabs.out`）。
典型值：

```
ControlType.SplitButton  粘贴        Alt, H, V
ControlType.Button       格式刷      Alt, H, F P
ControlType.Button       加粗        Alt, H, 1
ControlType.Button       筛选        Alt, A, F
ControlType.Button       冻结窗格    Alt, W, F
ControlType.TabItem      帮助        Alt, Y
```

说明 Excel 的组内 KeyTip 本来就是两级（`F` → `P`），不是单字母。

## 代码改动

1. `customUI.xml`：按「页签名 + 文案」把 Excel AccessKey 的页签后序列写回 90 个
   控件（`粘贴 V`、`格式刷 FP`、`加粗 1`、`筛选 F`、`冻结窗格 F`…），帮助页签
   改为 Excel 的 `Y`。
2. `ribbon/mod.rs`：XML 里没有 Excel 对应项的控件，解析时自动补一个页签内
   唯一的两位序列；第一位避开所有显式 keytip 的首字符，防止「按 F 执行了
   自研命令，导致 FP 永远到不了」。
3. `main.rs`：命令级 KeyTips 支持**前缀收窄**：
   - 输入 `F`：若有精确 `F` 命令则执行，否则收窄到 `F*`；
   - 再输入 `P`：精确匹配 `FP` 执行格式刷；
   - Backspace 退一级，Esc 退到页签级/关闭；
   - 数字键 `0`–`9` 也可作为 KeyTip（Excel 的加粗=1、斜体=2、下划线=3）。
4. `ribbon/ui.rs`：提示框显示完整序列，并按已输入前缀过滤。

## 机检证据

新增 3 个测试，全部通过（`cargo test -p app --bin app keytips_`）：

- `keytips_reach_border_filter_and_freeze_from_tabs`（原有 B/F/F 仍可执行）；
- `keytips_support_two_stage_sequences`：Home 按 `F` 后 `keytip_prefix=="F"`、
  格式刷**未**提前执行；再按 `P` 才置 `format_painter=Some`，随后状态清空；
- `keytips_cover_home_controls_uniquely`：Home 页所有带 onAction 的顶层控件
  都有非空 keytip，且无重复（>20 个）。

## 仍缺

- 真实物理键盘 Alt/方向键/Enter（本机 `GetForegroundWindow()=0`，SendInput 被拒）；
- 真实鼠标在右下角单元格右键（坐标边界已有 800×600 单测）。

所以 UI-23.1 仍待验收，但「只有边框/筛选/冻结三个控件有字母」已经消除。
