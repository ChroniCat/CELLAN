# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 计数以 `implementation-progress.json` 为准：`python scripts/audit_progress.py counts`。

## 当前状态（最后更新：2026-11-12 UTC+08，检查点）

- 分支 `main`，已推送 `origin/main`；最新提交 `e3d2bbf`
- 已验收：任务 **1/61**（UI-01）；切片 **7/148**
  （QA-03.1、UI-01.1、UI-02.1、UI-02.2、C13.1、C13.2、C13.3）
- 进行中父任务：QA-03（剩 3.2/3.3）、UI-02（剩 2.3）、C13（只剩图表注入夹具与文件占用场景）
- 工作区：干净
- 基线：sheetcore **460 passed / 0 failed / 34 ignored**；
  app 单测 **128 passed / 0 failed / 0 ignored / 4 filtered**；GUI 回归 8 passed / 9 ignored
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`（不在 PATH）
- 隔离运行：`source scripts/audit_env.sh <标签>`；清单工具 `python scripts/audit_progress.py`
- 证据：`docs/excel-wps-audit-2026-11-12/implementation-evidence/`

## 已完成（提交 → 证据）

| 切片 | 提交 | 证据 |
|---|---|---|
| QA-03.1 环境隔离 | `d1ebffd` | `qa03-1-*.txt` |
| UI-01.1 编辑提交（任务级已验收） | `b742b2c` | `ui01-edit-commit.txt`、`ui01-full-window-probe.rs` |
| UI-02.1 Ctrl+S / 另存为 | `6dc206a` | `ui02-1-save-shortcuts.txt`、`ui02-1-red.txt` |
| UI-02.2 Ctrl+O/N/P | `5535d40` | `ui02-2-open-new-print.txt`、`ui02-2-red.txt` |
| C13.1 吞错盘点 + 注入 + 首批传播 | `d3a141a` | `c13-1-swallowed-errors.txt` |
| C13.2 34 处吞错全部传播 | `7129fcd` | `c13-2-suite.txt`、`c13-1-swallowed-errors.txt` 末节 |
| C13.3 dirty/回执/重试 | `e3d2bbf` | `c13-3-save-failure-receipt.txt` |

要点：
- `save_plain_base` 已无 `eprintln!("[save] … 失败")`；`must_save!` 失败即中止，临时文件不替换目标。
- 注入开关 `SC_FAIL_SAVE_STEP=<名称>`；回归 `crates/core/tests/save_failure_injection.rs`（3 个测试，
  覆盖 33 无条件步骤 + 7 有条件步骤 + 宏工作簿）。
- 保存失败：状态栏带失败步骤、dirty 保留、磁盘不变、可重试。

## 下一步

**C13 收尾（补图表注入夹具）→ 然后 UI-02.3 命令注册表**

1. C13：造一份带真实图表的基准（`chart_write::chart_from_selection` + `insert_chart` 后用
   `xlsx::load` 读回，chart.drawing/part 非空），把 `charts_dirty=true` 后注入 `chart-anchor`，
   断言保存失败且原文件不变；再补一个文件被独占导致 rename 失败的场景。
2. 之后按 task-index：UI-02.3 命令 enabled/tooltip 统一注册（依赖 UI-02.1/2.2 已完成）。

## 尚未解决的风险 / 未执行

- 图表锚点/引用注入、文件占用/权限不足的 rename 失败：未执行（见上）
- 真实 IME、硬件键盘、系统对话框、DPI、打印、Excel/WPS 对拍：未执行（QA-04/05/09）
- 窗口关闭时 dirty 文档无保存提示：既有缺口，登记后续
- 应用测试需串行；网络测试默认跳过；Supabase 生产数据只读、本轮未接触
