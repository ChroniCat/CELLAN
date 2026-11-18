# C07.3 物理键盘数据验证下拉实测（2026-11-18）

## 环境

- 交互 RDP 会话 `rdp-tcp#0 Administrator 2 Active`。
- `work/probe/c07_int.xlsx` = `crates/core/tests/fixtures/c07/keyboard.xlsx` 副本。
- 物理注入：`SendInput(KEYEVENTF_SCANCODE)`；Alt scan `0x38`、Down scan `0x50`、Up scan `0x48`、Enter scan `0x1C`、Esc scan `0x01`。
- 不是 `SC_KEYSEQ`，不是 UIA 菜单项点击。

## 读数

### 1. 打开下拉

先物理点击 B2（列 B `372,463,72,19`，第 2 行 `271,502,29,20`，点 `408,512`），UIA：

```text
工作表 键盘，选区 B2:B2，1 行 1 列，活动单元格 B2
B2，香蕉，已选中
```

物理 Alt+↓ 后 UIA：

```text
苹果|ControlType.Button|377,567,37,24
香蕉|ControlType.Button|377,594,37,24
橙子|ControlType.Button|377,621,37,24
葡萄|ControlType.Button|377,648,37,24
```

截图：`c07-keyboard-1118.png`（下拉 4/4 项，香蕉高亮）。

### 2. ↓ + Enter

物理 Down 一次、Enter：

```text
编辑栏 = 香蕉
B2，香蕉，已选中
```

下拉关闭，B2 提交为 `香蕉`。

### 3. ↑ + Esc

再次 Alt+↓ 打开，按 Up、Esc：

```text
编辑栏 = 香蕉
B2，香蕉，已选中
```

无下拉项残留，选区值未变，Esc 取消生效。

### 4. Ctrl+S 保存

物理 Ctrl+S 后状态栏：

```text
已保存 work/probe/c07_int.xlsx
```

解包：

```xml
<c r="B2" t="s"><v>2</v></c>
```

`xl/sharedStrings.xml` 中 `香蕉` 为第 3 项（index 2），确认保存到文件。

## 结论

C07.3 的物理 Alt+↓ / ↑↓ / Enter / Esc 与 Ctrl+S 保存链路全部在真实交互会话中执行通过；此前“合成 Alt 修饰键不进入 egui”的结论已撤回，原因是旧注入在非交互会话里未真正送达窗口。

## 证据

- `docs/excel-wps-audit-2026-11-12/implementation-evidence/c07-keyboard-1118.png`
- `work/probe/c07_int.xlsx`
- `work/probe/c07_int.log`
