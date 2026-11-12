# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 计数以 `implementation-progress.json` 为准：`python scripts/audit_progress.py counts`。

## 当前状态（最后更新：2026-11-12 UTC+08，C13 门禁 + QA-03 复核补正后）

- 分支 `main`，跟踪 `origin/main`；最新提交 `1eea577`（本轮代码）与紧随其后的台账提交
- 已验收：任务 **3/61**（UI-02、C13、QA-03）；切片 **9/148**
- 待验收：UI-01（真实中文输入法与硬件键盘未执行，父任务与 UI-01.1 都保持待验收）
- 基线（本轮实测）：
  - app 单测 **139 passed / 0 failed / 0 ignored**，**默认并行连跑两轮 EXIT=0**
  - GUI `ui_regression` **12 passed / 0 failed / 8 ignored**（含 2 个真实系统对话框用例、1 个双进程持锁用例）
  - sheetcore **525 passed / 0 failed / 34 ignored**（54 个测试二进制，EXIT=0）
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`；`source scripts/audit_env.sh <标签>`

## 本轮完成（提交 → 证据）

| 内容 | 提交 | 证据 |
|---|---|---|
| C13 最终产物门禁（ZIP/CFB 识别、关系归一、XML 单根、prune 修复） | `1eea577` | `c13-integrity-gate.txt` |
| C13 真实占用（Windows 独占句柄）+ 保存阶段强杀子进程 | `1eea577` | `c13-integrity-gate.txt` |
| QA-03.1 环境竞态修复 + 并行 GREEN | `1eea577` | `qa03-1-env-race-fix.txt` |
| QA-03.3 free_port TOCTOU + 他人持锁绘制 | `1eea577` | `qa03-3-other-participant-lock.txt` |
| UI-02 真实系统另存为/打开对话框 + 原文件哈希 | `1eea577` | `ui02-real-dialogs.txt` |

## 台账口径（不要凭记忆）

- `counts`：`tasks.已验收=3`、`slices.已验收=9`、`待验收=1`（UI-01 及其切片）
- `check` 输出「清单一致」，说明 tasks.json 的 61/148 与台账结构未走样

## 下一步（按依赖优先级）

1. **UI-01 收尾**：真实中文输入法（组合中取消、候选窗上屏）与真实硬件键盘的 Alt+Enter 光标位置。
   必须真机人工执行，事件注入不算；做完前 UI-01 保持待验收。
2. 之后按 `task-index.md` 的 P0 顺序推进未开工任务（C 系列 → UI 系列 → QA 系列），
   每完成一个切片就写证据 + 更新台账 + 提交；父任务只在全部切片与整体验收通过后才标已验收。

## 尚未解决的风险 / 未执行

- 物理断电不可模拟（用保存阶段强杀进程代替，边界已在证据里写明）
- 真实 IME、硬件键盘、DPI、打印、Excel/WPS 对拍、跨机 LAN 协作：未执行
- 8 个 GUI 用例仍依赖 `tests/test-work` 外部夹具（ignored）
- 窗口关闭时 dirty 文档无保存提示：既有缺口，登记后续
- 网格级 Ctrl+C/X/V/F/H 未进命令表（留 UI-06/07）
- Supabase 生产数据只读、未接触
