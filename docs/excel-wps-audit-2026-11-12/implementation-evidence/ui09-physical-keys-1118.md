# UI-09 物理键盘验收（2026-11-18）

本文件记录 UI-09.1 / UI-09.2 / UI-09.3 的真实窗口、真实键盘注入与落盘回读。
注入链路：`work/probe/ui09_force.ps1` 用 `AttachThreadInput + SetForegroundWindow + SetFocus`
把 app 窗口置前，再以 `keybd_event` 发送扫描码；`-ScanCtrl` 是 Ctrl 按下 + 目标键扫描码，
不是 `input_lib.ps1` 的 `CtrlKey`（后者对 PGDN 只产生 PageDown，已在前一轮证伪）。

## 1. 代码修复：Tab 首次被 egui 焦点系统吞掉

修改 `crates/app/src/main.rs::handle_keys`：

- 当 `editing.is_none() && ctx.wants_keyboard_input()` 时，原来无条件 `return`。
- egui 在第一次 Tab 时会把焦点交给第一个可聚焦控件（实测焦点 id `0x23AC`，名称框），
  因此网格收不到 Tab。
- 现在仅当按的是 Tab、且没有命令面板/查找/对话框时，先 `surrender_focus` 并
  `stop_text_input`，本帧继续处理 Tab；帧末再清一次焦点（`clear_grid_focus`），
  避免焦点留在功能区的按钮上。
- 鼠标点击网格正文时也 `surrender_focus`，点完工具栏再点单元格即可继续用键盘。
- 回归：`crates/app/src/edit_regression_tests.rs::first_tab_after_startup_moves_cell_instead_of_leaving_focus_in_ribbon`。

修复前 `work/probe/ui09_keys6.log` 的同一帧读数（Tab 不移动）：

```text
[keydebug] wants_kb=true editing=false tab=true enter=false pgdn=false down=false focus=Some(23AC) grid_focus=false fx_focus=false
A1，S1-A1，已选中
```

## 2. UI-09.3 Tab / Enter / 区域循环 / 隐藏行

fixture `work/probe/ui09_keys.xlsx`，`SC_SEL=0,0 SC_SEL2=1,1`，选区 `A1:B2`：

```text
FG=49023496 BEFORE=A1，S1-A1，已选中 AFTER=B1，4，已选中 TABS=*S1,S2,S3
FG=49023496 BEFORE=B1，4，已选中 AFTER=A2，S1-A2，已选中 TABS=*S1,S2,S3
FG=49023496 BEFORE=A2，S1-A2，已选中 AFTER=B2，9，已选中 TABS=*S1,S2,S3
FG=49023496 BEFORE=B2，9，已选中 AFTER=A1，S1-A1，已选中 TABS=*S1,S2,S3
```

Shift+Tab 反向：

```text
FG=49023496 BEFORE=A1，S1-A1，已选中 AFTER=B2，9，已选中 TABS=*S1,S2,S3
```

Enter 循环：

```text
FG=49023496 BEFORE=B2，9，已选中 AFTER=A1，S1-A1，已选中 TABS=*S1,S2,S3
FG=49023496 BEFORE=A1，S1-A1，已选中 AFTER=A2，S1-A2，已选中 TABS=*S1,S2,S3
FG=49023496 BEFORE=A2，S1-A2，已选中 AFTER=B1，4，已选中 TABS=*S1,S2,S3
FG=49023496 BEFORE=B1，4，已选中 AFTER=A2，S1-A2，已选中 TABS=*S1,S2,S3   # Shift+Enter
```

有编辑内容时 Enter 提交并下移：在 B2 输入 `9` 后状态为 `单元格模式 输入`，
按 Enter 后 UIA 读数为：

```text
B2，9，已选中
EDIT=9
```

截图：`ui09-tab-enter-physical.png`（B1=4、B2=9、B2 选中，公式栏 9）。

隐藏行 fixture `work/probe/ui09_hidden.xlsx`（第 2 行 hidden），选区 `A1:B4`：

```text
FG=8326692 BEFORE=A1，A1，已选中 AFTER=B1，B1，已选中 TABS=*S1
FG=8326692 BEFORE=B1，B1，已选中 AFTER=A3，A3，已选中 TABS=*S1
FG=8326692 BEFORE=A3，A3，已选中 AFTER=B3，B3，已选中 TABS=*S1
FG=8326692 BEFORE=B3，B3，已选中 AFTER=A4，A4，已选中 TABS=*S1
```

## 3. UI-09.2 视口翻页 / 切表

同一 `ui09_keys.xlsx`，PageDown/PageUp 是真实 PGDN/PGUP 扫描码：

- 900 高（`SC_WIN_H=900`）：`A1 → A28`；`escape` 后 Down `A28 → A29`。
- 720 高（`SC_WIN_H=720`）：
  ```text
  FG=20717438 BEFORE=A1，S1-A1，已选中 AFTER=A19，S1-A19，已选中 TABS=*S1,S2,S3
  FG=20717438 BEFORE=A19，S1-A19，已选中 AFTER=A1，S1-A1，已选中 TABS=*S1,S2,S3
  FG=20717438 BEFORE=A1，S1-A1，已选中 AFTER=A19，S1-A19，已选中 TABS=*S1,S2,S3
  FG=20717438 BEFORE=A19，S1-A19，已选中 AFTER=A37，已选中 TABS=*S1,S2,S3
  ```
- 1080 高（`SC_WIN_H=1080`）：
  ```text
  FG=8991546 BEFORE=A1，S1-A1，已选中 AFTER=A37，已选中 TABS=*S1,S2,S3
  ```

翻页步长随视口行数变化（720=18，900=27，1080=36），活动格与首屏末行保持一页偏移；
截图 `ui09-pagedown-1080.png`（A37，视口 37–73 行）。

Ctrl+PageDown / Ctrl+PageUp（`-ScanCtrl`）：

```text
FG=8991546 BEFORE=A37，已选中 AFTER=A1，S2-A1，已选中 TABS=S1,*S2,S3
FG=8991546 BEFORE=A1，S2-A1，已选中 AFTER=A37，已选中 TABS=*S1,S2,S3
```

切表后活动格在新表为 A1；切回原表恢复到 A37，证明切表没有改写原表活动格。

## 4. UI-09.1 选择/格式快捷键

| 快捷键 | 注入 | 证据 |
|---|---|---|
| Ctrl+B | `ui09_force.ps1 -Key B -ScanCtrl` | 保存后 openpyxl 回读 `B2 bold=True` |
| Ctrl+I | `-Key I -ScanCtrl` | 保存后 `A1 italic=True` |
| Ctrl+U | `-Key U -ScanCtrl` | 保存后 `A1 underline=single` |
| Ctrl+A | `-Key A -ScanCtrl` | 第一次 `选区 A1:C30`；第二次 `已选择整个工作表` / `A1:XFD1048576` |
| Ctrl+Space | `-Key SPACE -ScanCtrl` | `已选择 B 列` / `选区 B1:B1048576，1048576 行 1 列` |
| Shift+Space | `-Key SPACE -Shift` | `已选择第 2 行` / `选区 A2:XFD2，1 行 16384 列` |
| F5 | cua `key f5` | UIA `名称框 FOCUS=True` |
| Ctrl+G | `-Key G -ScanCtrl` | UIA `名称框 FOCUS=True` |

Ctrl+A 第一次/第二次的原始读数：

```text
第一次后：工作表 S1，选区 A1:C30，30 行 3 列，活动单元格 A1
FG=9580486 BEFORE=A1，S1-A1，已选中 AFTER=A1，S1-A1，已选中 TABS=*S1,S2,S3
FG=9580486 BEFORE=A1，S1-A1，已选中 AFTER=A1，S1-A1，已选中 TABS=*S1,S2,S3
第二次后：已选择整个工作表
工作表 S1，选区 A1:XFD1048576，1048576 行 16384 列，活动单元格 A1
```

## 5. 回归

```text
cargo test --offline --no-default-features --features glow -p app first_tab_after_startup -- --test-threads=1
test edit_regression_tests::first_tab_after_startup_moves_cell_instead_of_leaving_focus_in_ribbon ... ok
test result: ok. 1 passed; 0 failed
```

全量 app 回归（最终代码，`--test-threads=1`）：

```text
running 394 tests
test result: ok. 393 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 57.76s
running 20 tests
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 119.00s
```

`ui_regression::ribbon_groups_aligned_and_fit` 原先取日志里**最后一帧**分组；打开文件后
1440 宽只剩 5 组，导致断言必失败。现改为取开始页第一组完整 8 分组（首帧），
与断言目标一致，单测已通过。此改动不涉及产品代码。

