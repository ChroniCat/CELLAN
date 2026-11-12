# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 不把自然语言总结当作已完成证据；计数以 `implementation-progress.json` 为准
> （`python scripts/audit_progress.py counts`）。

## 当前状态（最后更新：2026-11-12）

- 分支：`main`（跟踪 `origin/main`，会话开始时 ahead 1）
- 会话开始提交：`1f4683b docs: audit Excel and WPS parity with prioritized implementation tasks`
- 工作区：本记录写入时干净；本轮改动见下方「本轮改动」与 `git status`
- 进度清单：`docs/excel-wps-audit-2026-11-12/implementation-progress.json`
  （61 任务 / 148 切片，初始全部「待实施」）
- 证据目录：`docs/excel-wps-audit-2026-11-12/implementation-evidence/`
- 隔离运行：`source scripts/audit_env.sh <标签>`（TEMP/TMP/LOCALAPPDATA 隔离，清 SC_*）
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`（cargo 1.99.1）；`cargo` 不在 PATH
- 进度工具：`python scripts/audit_progress.py init|set|slice|show|counts|check`

## 当前任务

**QA-03.1** 临时目录与环境恢复（父任务 QA-03 P0，依赖无）。

已确认的问题（源码 + 已有证据）：

1. `crates/app/src/recent.rs:63-87` 的测试改进程 `LOCALAPPDATA` 后不恢复，后续测试会读到错误目录。
2. 应用/测试没有统一的测试环境构造器：各处自己拼 `std::env::temp_dir()` + 固定文件名，
   运行目录互相覆盖，也不清理继承的 `SC_*`。
3. `crates/app/tests/ui_regression.rs` 启动 app.exe 时只 `env_remove` 了 SC_B1/B3/B4，
   其他继承的 `SC_*`（如 SC_DUMP/SC_SHOT/SC_SEL）仍可影响子进程；子进程的
   `LOCALAPPDATA` 仍是真实用户目录（打开工作簿会写 recent.txt）。

正在验证的假设：上述三点可由一个「唯一测试环境守卫 + 子进程环境隔离」的改动闭合，
且不改变生产行为。

下一步动作：实现 `crates/app/src/test_env.rs`（cfg(test) 守卫），迁移 recent/error_opts 测试，
让 `ui_regression` 子进程使用本次运行专用目录；然后用污染环境跑套件并核对 canary 哈希。

## 已执行命令及结果

（本轮开始）

- `git status --short && git branch -vv` → 干净，`main` ahead 1
- `python scripts/audit_progress.py init` → 61 任务 / 148 切片写入清单
- `python scripts/audit_progress.py check` → 清单一致

## 尚未解决的风险

- 148 个切片工作量很大；每轮只推进有界切片，检查点写回本文件。
- 应用测试会启动真实窗口（单实例端口），必须串行；网络测试默认跳过，涉及协作的任务需另跑隔离回环。
- Supabase/生产数据只读，本轮不需要接触。
