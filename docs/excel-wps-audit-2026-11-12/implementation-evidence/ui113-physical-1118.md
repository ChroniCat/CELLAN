# UI-11.3 引用拾取 / F4 / 引用颜色（真实鼠标与键盘，2026-11-18）

真实窗口：`ui09_keys.xlsx`，`SC_INSTANCE_PORT=49978`、`SC_SEL=0,0`、1440×900。
输入：`computer_use type/key`（SendInput 真实输入）；鼠标为
`ui09_force.ps1 -Key NONE`（AttachThreadInput 强制前台）后
`input_lib.ps1 Click-At`（真实 `mouse_event`）。

## 1. 编辑公式时点 B2 = 插入引用，不提交

初始活动格 `A1`，逐字符键入 `=A1`：

```text
EDIT==A1
```

用 UIA 读取列标/行号几何：

```text
列 B|579,593,149,19
第 2 行|401,632,29,20
```

即 B2 中心 `(579+74, 632+10) = (653,642)`。真实鼠标点击后：

```text
A1，S1-A1，已选中
EDIT==B2
STATUS=单元格模式 输入
```

- 活动格仍是 `A1`，没有提交、没有搬到 B2；
- 公式栏由 `=A1` 变成 `=B2`；
- 状态栏 `已插入引用 B2`；
- 截图 `ui113-reference.png`：名称框仍是 `A1`，A1 处于编辑态，**B2 带蓝色引用边框**。

## 2. F4 循环

同一干净会话，`=A1` 起，连续四次真实 F4，逐次 UIA 回读：

```text
F4 #1  EDIT==$A$1
F4 #2  EDIT==A$1
F4 #3  EDIT==$A1
F4 #4  EDIT==A1
```

另一会话对 `=B2` 做同样循环：

```text
EDIT==B2 → $B$2 → B$2 → $B2 → B2
```

## 3. 引用颜色

`ui113-reference.png` 中 B2 为蓝色引用边框，A1 仍是绿色编辑框；
名称框只显示 `A1`，说明点 B2 只是取引用，不是切换活动格。

## 4. 仍未做

真实按住拖动跨表建 `Sheet2!B2:C4` 未验（本机交互桌面无法稳定保持按住拖动）；
代码路径与单测已覆盖（切表清锚点 + 带表名/引号的 `pick_range_label`），
保留在 `not_done`。
