# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 计数以 `implementation-progress.json` 为准：`python scripts/audit_progress.py counts`。

## 当前状态（最后更新：2026-11-12 UTC+08，检查点）

- 分支 `main`，已推送 `origin/main`；最新提交 `d3a141a`
- 已验收：任务 **1/61**（UI-01）；切片 **5/148**（QA-03.1、UI-01.1、UI-02.1、UI-02.2、C13.1）
- 进行中父任务：QA-03（剩 3.2/3.3）、UI-02（剩 2.3）、C13（剩 13.2/13.3）
- 工作区：干净
- 基线：sheetcore **458 passed / 0 failed / 34 ignored**；
  app 单测 **127 passed / 0 failed / 0 ignored / 4 filtered**；
  GUI 回归 **8 passed / 0 failed / 9 ignored**
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`（不在 PATH）
- 隔离运行：`source scripts/audit_env.sh <标签>`
- 清单工具：`python scripts/audit_progress.py {init,set,slice,show,counts,check}`
- 证据目录：`docs/excel-wps-audit-2026-11-12/implementation-evidence/`

## 已完成（提交与证据）

| 切片 | 提交 | 证据 |
|---|---|---|
| QA-03.1 临时目录与环境恢复 | `d1ebffd` | `qa03-1-before-source.txt` / `qa03-1-env-isolation.txt` / `qa03-1-ui-child.txt` |
| UI-01.1 编辑提交链路（任务级也已验收） | `b742b2c` | `ui01-edit-commit.txt` / `ui01-full-window-probe.rs` |
| UI-02.1 Ctrl+S / 另存为 | `6dc206a` | `ui02-1-save-shortcuts.txt` / `ui02-1-red.txt` |
| UI-02.2 Ctrl+O/N/P | `5535d40` | `ui02-2-open-new-print.txt` / `ui02-2-red.txt` |
| C13.1 保存吞错盘点 + 故障注入 + 首批 5 处传播 | `d3a141a` | `c13-1-swallowed-errors.txt` / `c13-1-core-suite.txt` / `c13-1-app-suite.txt` |

要点：
- 编辑缓冲唯一所有者 `self.editing`，`commit_edit -> bool` 只在实际写入后移动选区；格内/编辑栏/
  Esc/切表/关窗统一入口；编辑栏整行可点、固定 id、当帧取焦点。
- 保存快捷键与标题栏/功能区共用 `save(false)`/`save(true)`；另存为与打开对话框支持
  `SC_SAVE_AS`/`SC_OPEN_PATH` 只替换对话框（无界面验收）。
- 保存子步骤：`SC_FAIL_SAVE_STEP=<名称>` 注入失败 + `must_save!` 失败即中止；
  已传播 merge-sheet-data / layout / protection / page-setup / fixups-migrate-sheet-rels。

## 下一步（按 task-index 建议顺序）

**C13.2 错误传播和临时文件完整性**（父任务 C13 P0）

- 盘点表在 `implementation-evidence/c13-1-swallowed-errors.txt`，还剩 29 处只 eprintln
- 第一条动作：把无条件执行的 `apply_outline_pr/apply_scenarios/apply_ignored_errors/
  apply_tab_colors/hyperlinks/sparkline/textbox/apply_sheet_states/apply_rich/table::sync`
  改为 `must_save!`，扩展 `save_failure_injection.rs` 的步骤表并跑绿
- 有条件触发的（宏/图表/表格/CF/外链/未编辑表恢复/rel 收尾）需要先造带该部件的合成基准，
  再注入；不能只测空转路径

## 尚未解决的风险 / 未执行

- 真实 IME、硬件键盘、系统另存为/打开对话框、DPI、打印、Excel/WPS 逐项对拍：未执行（QA-04/05/09）
- 窗口关闭时 dirty 文档无保存提示：既有缺口，已登记后续
- 其余 29 处保存吞错仍在（C13.2），在此之前不能宣称保存失败已全部可信
- 应用测试需串行；网络测试默认跳过，协作任务需另跑隔离回环
- 本机 Supabase 生产数据只读，本轮未接触
