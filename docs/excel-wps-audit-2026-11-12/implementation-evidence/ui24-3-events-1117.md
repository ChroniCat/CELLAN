# UI-24.3 证据：UIA SelectionItem 选中事件真实回传（2026-11-17）

## 之前卡在哪

修好 `SelectionItemPattern` 的 `IsSelected` 读回后，用 PowerShell 写的事件监听
（`AddAutomationEventHandler` / `AddAutomationFocusChangedEventHandler`，
含 `-STA` + `Application.DoEvents`）一直是 0 回调，台账因此把
「UIA 选中事件回调未能在本会话观测」留成唯一 not_done。

## 这次的做法

改用 C#（.NET Framework 4.x）编译一个 STA 控制台探针
`scripts/uia_event_probe.cs`：

- 按 **PID** 找 app 主窗口（`AutomationElement.RootElement.FindFirst` +
  `ProcessIdProperty`）；
- 在窗口子树上注册 `SelectionItemPattern.ElementSelectedEvent` 和
  `AutomationFocusChangedEventHandler`；
- 用 WinForms `Application.Run()` 跑真正的 STA 消息泵（PowerShell 的
  `DoEvents` 循环不是同一个东西）；
- 20 秒后退出。

编译（GAC 里的 UIAutomation 程序集）：

```
csc -out:UiaEventProbe.exe \
  -r:UIAutomationClient.dll -r:UIAutomationTypes.dll \
  -r:System.Windows.Forms.dll -r:WindowsBase.dll scripts/uia_event_probe.cs
```

运行：app 打开 `crates/core/tests/fixtures/qa09/a11y.xlsx`，
`SC_SEL_STEP=700:0,0;1,0;2,0;3,0;0,0;1,0` 每 700ms 移动选区，
探针按 PID 挂上事件后打印。

## 实测输出（`ui24-3-events.txt`）

```
WINDOW a11y.xlsx - Sheets Native
EVENT ControlType.DataItem | A1，合并标题，已选中
EVENT ControlType.DataItem | A2，已选中
EVENT ControlType.DataItem | A3，已选中
EVENT ControlType.DataItem | 第 3 行，已选中
EVENT ControlType.DataItem | A4，已选中
EVENT ControlType.DataItem | 第 4 行，已选中
EVENT ControlType.DataItem | A1，合并标题，已选中
...
```

- **24 条 `ElementSelected` 事件**，覆盖选区在 A1/A2/A3/A4 之间来回移动；
- 事件源是 `ControlType.DataItem`（`Role::Cell` 映射到 UIA 的 DataItem），
  Name 里带「已选中」；行头节点也会随选区发事件；
- `AutomationFocusChangedEventHandler` 仍为 0：`SC_SEL_STEP` 只移动选区、
  不移动键盘焦点，所以这不是缺陷，只是本轮没有制造焦点变化。

## 结论

UI-24.3 的「网格坐标值 + 选区语义 + 选中事件」三项都有真实 UIA 读数/回调：

- 读回：A1 `IsSelected=True`、`HelpText=第 1 行，第 1 列，合并 A1:B2，冻结窗格内`；
- 事件：24 次 `ElementSelected`，源为 DataItem，Name 随选区变化；
- vendor 补丁（`accesskit_windows` 放行 Cell 的 SelectionItemPattern 并在
  `node_added` 补发 ElementSelected）被真实 UIA 客户端消费到。

真实屏幕阅读器（NVDA/讲述人）与物理鼠标路径仍归 UI-24.2 的阻塞，不影响本项。
