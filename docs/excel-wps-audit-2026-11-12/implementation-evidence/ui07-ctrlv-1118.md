# UI-07 / UI-07.1 物理 Ctrl+V 实测（2026-11-18）

## 环境

- 交互会话：`query session` 显示 `rdp-tcp#0 Administrator 2 Active`。
- app 窗口真实前台：`FG=21110590`，标题 `int_paste2.xlsx - Sheets Native`。
- 注入方式：`SendInput(KEYEVENTF_SCANCODE)`，Ctrl scan `0x1D`、V scan `0x2F`、S scan `0x1F`；不是 `SC_KEYSEQ`，也不是 UIA Ribbon 按钮。
- 工作簿：`work/probe/int_paste2.xlsx`（`crates/core/tests/fixtures/c10/table.xlsx` 副本）。

## 关键发现

物理 `Ctrl+V` 在 Windows 上被 winit/egui 归一成 `egui::Event::Paste("...")`，**不产生 `Key::V` 按键事件**。原代码只在 `i.key_pressed(Key::V)` 时进入粘贴分支，所以真实的系统粘贴事件被直接丢掉；这不是 Ctrl 修饰键没进 egui，而是入口判断漏了 `Event::Paste`。

同一现象也出现在 `Ctrl+C`/`Ctrl+X`：系统复制/剪切分别以 `Event::Copy`/`Event::Cut` 到达。

## 修复

`crates/app/src/main.rs`：

```rust
let (copy_evt, cut_evt, paste_evt) = ctx.input(|i| {
    (
        i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
        i.events.iter().any(|e| matches!(e, egui::Event::Cut)),
        i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
    )
});
...
if copy_k || copy_evt { self.clip_copy(false, ctx); return; }
if cut_k || cut_evt { self.clip_copy(true, ctx); return; }
if paste_k || paste_v_k || paste_evt {
    // 原有 HTML/TSV/系统文本粘贴逻辑
}
```

回归测试：`crates/app/src/edit_regression_tests.rs`

```text
system_paste_event_commits_without_key_v
```

只发 `egui::Event::Paste("PASTE_EVENT")`、不发 `Key::V`，断言 A1 == `PASTE_EVENT` 且不残留编辑态。

## 真实现场读数

### 1. 物理 Ctrl+V 写入 A1

先用真实键盘扫描码验证 Ctrl 组合生效（不是按住 Ctrl 的单一字符）：

```text
Ctrl+A: 名称框 A1 -> A1:C4
```

随后：

```text
Set-Clipboard 'PASTE_SAVE'
ui09_force.ps1 -Title int_paste2 -ScanCtrl -Key V
FG=21110590
BEFORE=A1，Amount，已选中
AFTER=A1，PASTE_SAVE，已选中
```

UIA：

```text
名称框 = A1
编辑栏 = PASTE_SAVE
```

保存后解包：

```xml
<c r="A1" t="s"><v>3</v></c>
```

`xl/sharedStrings.xml` 第 4 项为 `PASTE_SAVE`，证明文本已写入工作簿，不只是界面回显。

### 2. 物理 Ctrl+C / Ctrl+V 跨格

```text
A1=PASTE_FIXED
Ctrl+C -> Get-Clipboard = PASTE_FIXED
点击 B2 -> Ctrl+V
B2，PASTE_FIXED，已选中
编辑栏 = PASTE_FIXED
```

### 3. 入口一致性

- Ribbon `粘贴`：此前已由 UIA Invoke 验证（A1=PASTE_RCLICK）。
- 右键 `粘贴`：`ui07-rightclick-1118.md` 的硬件 `mouse_event` 验证（A1=PASTE_RCLICK2）。
- 物理 `Ctrl+V`：本轮 A1=PASTE_SAVE / B2=PASTE_FIXED。
- 三者都落到同一个 `clip_paste`，读同一份系统剪贴板。

## 测试

```text
cargo test --offline --no-default-features --features glow -j2 -p app --bin app system_paste_event -- --test-threads=1
=> 1 passed

cargo test --offline --no-default-features --features glow -j2 -p app --bin app -- --test-threads=1
=> 395 passed; 0 failed; 1 ignored; 63.74s
```

## 证据文件

- `docs/excel-wps-audit-2026-11-12/implementation-evidence/ui07-ctrlv-1118.png`
  （截图：名称框 A1，编辑栏 `PASTE_SAVE`，状态栏“已保存 work/probe/int_paste2.xlsx”）
- `work/probe/int_paste2.xlsx`
- `work/goal-logs/app-paste-event-1118.log`
- `work/goal-logs/app-paste-full-1118.log`
