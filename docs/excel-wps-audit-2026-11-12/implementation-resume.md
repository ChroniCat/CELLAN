# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 计数以 `implementation-progress.json` 为准：`python scripts/audit_progress.py counts`。

## 当前状态（最后更新：2026-11-12 UTC+08，检查点）

- 分支 `main`，已推送 `origin/main`；最新提交 `5728dc2`
- 已验收：任务 **2/61**（UI-01、C13）；切片 **7/148**
  （QA-03.1、UI-01.1、UI-02.1、UI-02.2、C13.1、C13.2、C13.3）
- 进行中父任务：QA-03（剩 3.2/3.3）、UI-02（剩 2.3）
- 工作区：干净
- 基线：sheetcore **462 passed / 0 failed / 34 ignored**；
  app 单测 **128 passed / 0 failed / 0 ignored / 4 filtered**；GUI 回归 8 passed / 9 ignored
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`；隔离：`source scripts/audit_env.sh <标签>`
- 清单：`python scripts/audit_progress.py`；证据：`implementation-evidence/`

## 已完成（提交 → 证据）

| 切片/任务 | 提交 | 证据 |
|---|---|---|
| QA-03.1 | `d1ebffd` | `qa03-1-*.txt` |
| UI-01（任务级） | `b742b2c` | `ui01-edit-commit.txt` |
| UI-02.1 | `6dc206a` | `ui02-1-*.txt` |
| UI-02.2 | `5535d40` | `ui02-2-*.txt` |
| C13.1 | `d3a141a` | `c13-1-swallowed-errors.txt` |
| C13.2 | `7129fcd` | `c13-2-suite.txt` |
| C13.3 + C13 父任务 | `e3d2bbf`、`5728dc2` | `c13-3-save-failure-receipt.txt`、`c13-parent-acceptance.txt` |

C13 结论文档：保存 34 处吞错已清零；注入 5 类（保护/图表/布局/CF/外链/宏）+ 只读目标全部
断言 Err、原文件哈希不变、重试成功；app 层 dirty 保留与可定位错误已验。

## 下一步

**UI-02.3 命令 enabled/tooltip 统一注册**（父任务 UI-02 的最后切片）

第一条动作：读 `crates/app/src/ribbon/ui.rs` 与 `ribbon/actions.rs`，列出当前按钮
enabled/tooltip 的来源（是否有集中表），先写出「同一命令在不同入口 enabled/提示不一致」
的最小复现，再建命令表并接线；快捷键（已完成的 Ctrl+S/O/N/P）改为按命令 id 分发。

## 尚未解决的风险 / 未执行

- 断电不可模拟；Office 真实独占锁未构造（只读目标覆盖同一 rename 失败分支）
- 真实 IME、硬件键盘、系统对话框、DPI、打印、Excel/WPS 对拍：未执行（QA-04/05/09）
- 窗口关闭时 dirty 文档无保存提示：既有缺口，登记后续
- 应用测试需串行；网络测试默认跳过；Supabase 生产数据只读、未接触
