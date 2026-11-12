# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 不把自然语言总结当作已完成证据；计数以 `implementation-progress.json` 为准
> （`python scripts/audit_progress.py counts`）。

## 当前状态（最后更新：2026-11-12 UTC+08）

- 分支：`main`，已推送 `origin/main`；最新提交 `b742b2c`
- 已验收：任务 1/61（UI-01）；切片 2/148（QA-03.1、UI-01.1）；QA-03 父任务进行中
- 工作区：干净（提交后核对）
- 进度清单：`docs/excel-wps-audit-2026-11-12/implementation-progress.json`
- 证据目录：`docs/excel-wps-audit-2026-11-12/implementation-evidence/`
- 隔离运行：`source scripts/audit_env.sh <标签>`（TEMP/TMP/LOCALAPPDATA 隔离，清 SC_*）
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`（cargo 1.99.1）；`cargo` 不在 PATH
- 进度工具：`python scripts/audit_progress.py init|set|slice|show|counts|check`
- 构建：`cargo build -p app --no-default-features --features glow --offline`
- 当前基线：应用单测 **120 passed / 0 failed / 0 ignored / 4 filtered**；
  GUI 回归 **8 passed / 0 failed / 9 ignored**

## 已完成（含证据）

1. **QA-03.1 临时目录与环境恢复（切片已验收）** — 提交 `d1ebffd`
   - `crates/app/src/test_env.rs`：唯一目录、TEMP/TMP/LOCALAPPDATA 隔离、Drop 恢复、
     清空继承 SC_*、只删自己目录；`scratch(tag)` 给只需唯一目录的夹具
   - recent/error_opts/files/session_lock/ui_prefs 测试迁移；ui_regression 子进程隔离环境
   - 证据：`implementation-evidence/qa03-1-before-source.txt`、`qa03-1-env-isolation.txt`、
     `qa03-1-ui-child.txt`（污染环境串行/并行一致；canary 与真实用户目录哈希不变）

2. **UI-01 真实键盘编辑提交链路（任务 + 切片已验收）** — 提交 `b742b2c`
   - 根因：`draw_grid_pane` 把 `self.editing` take 出来后 `commit_edit` 再 take 得 None，
     不写值/不记撤销/不标 dirty，只剩移动选区
   - 修法：`EditState{buf,spot}` 唯一所有者；`commit_edit -> bool` 只在实际写入后移动选区；
     `cancel_edit`；格内/编辑栏/Esc/切表/关窗统一入口；编辑栏整行可点、固定 id、当帧取焦点
   - 15 项整窗 `eframe::App::update` 事件回归（Enter/Tab/Shift/Esc/F2/编辑栏/点格/跳转/关窗/
     验证失败/撤销重做/保存重开/中文/编辑栏 Esc）
   - 证据：`implementation-evidence/ui01-edit-commit.txt`、`ui01-full-window-probe.rs`
   - 未执行：真实硬件键盘、中文 IME 候选（归 QA-04）；窗口关闭未保存提示（后续项）

## 进行中：C13.1 保存吞错盘点与故障注入

- 已加 `xlsx.rs::save_step` + `must_save!`：`SC_FAIL_SAVE_STEP=名称` 注入失败；未设置不改变行为
- 已把 5 个无条件子步骤改为失败即中止：merge-sheet-data / layout / protection / page-setup /
  fixups-migrate-sheet-rels（临时文件不再被 rename 成目标文件）
- 新回归 `crates/core/tests/save_failure_injection.rs`：注入→Err、错误点名步骤、原文件 SHA256 不变、
  清注入后重试成功（已通过）
- 盘点表（34 处吞错）：`implementation-evidence/c13-1-swallowed-errors.txt`
- 正在跑 `cargo test -p sheetcore --offline -- --test-threads=1` 全量回归；结果决定是否 13.1 收尾
- 剩余 29 处（宏/图表/表格/CF/外链/未编辑表恢复等）属 C13.2，需补对应合成夹具后逐个传播

## 下一步（按 task-index 建议顺序）

**UI-02.1 Ctrl+S / 另存为与编辑态**（父任务 UI-02 P0；依赖 UI-01 已满足）。

- 现状（ui-requirements.md UI-02）：`handle_keys` 主分发没有 Ctrl+S / Ctrl+Shift+S /
  Ctrl+O / Ctrl+N / Ctrl+P；帮助与标题提示却宣称 Ctrl+S 可用
- 本切片验收：隔离副本改 A1 按 Ctrl+S 后文件与重开值正确、dirty 消失；Ctrl+Shift+S
  弹另存为且不改原文件；编辑期间按保存先提交有效编辑（验证失败则不保存）
- 第一条动作：整窗事件测试发 Ctrl+S，确认当前无任何效果（RED），再接入统一命令入口

## 尚未解决的风险 / 未执行

- 真实 IME、硬件键盘、DPI、打印、Excel/WPS 逐项对拍：未执行（QA-04/05/09），不得写成通过
- 窗口关闭时 dirty 文档没有保存提示：既有缺口，不在本轮 61 项内，已登记后续
- 148 切片工作量大；每轮只推进有界切片并写回本文件
- 应用测试会起真实窗口/单实例端口，必须串行；网络测试默认跳过，协作任务需另跑隔离回环
- 本机 Supabase 为真实生产数据，默认只读；本轮未接触
