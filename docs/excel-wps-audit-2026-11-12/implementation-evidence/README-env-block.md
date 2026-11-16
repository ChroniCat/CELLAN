
## 交互桌面阻塞（2026-11-17 复核）

- `query session`：`rdp-tcp#0 Administrator 2 Active`，`explorer` 也在 Session 2；
- 但 `GetForegroundWindow() = 0`（`GetShellWindow()` 有值）；
- `Start-Process notepad` 直接失败：`This command cannot be run completely because the system cannot find all the information required`；
- 因此：`SendInput` / `SendKeys` / `WM_CHAR` / 真实鼠标点击 / 窗口截图（CopyFromScreen 之前也报 invalid handle）**全部不可用**。

影响范围（这些项的「真人点击/截图」验收都无法在本机完成）：

QA-04.1、QA-04.2、QA-07.1（冷启动需清系统缓存）、UI-11（真人拖动/点选）、UI-11.1（IME）、
UI-24.1（物理 Tab）、UI-29 父项（三端截图）、UI-30.1（283 动作点击矩阵中 14 个人工动作）。

解除方式（任一）：

1. 在该机器上用 RDP 保持**已连接且未最小化**的会话，让前台窗口存在；
2. 或提供一台可交互的机器，跑 `work/probe/qa04/*.ps1`；
3. 或由人工按清单执行并回传结果（我把清单整理成可勾选表格即用）。
