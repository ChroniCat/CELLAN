# 实施恢复记录

> 每轮开始、上下文压缩后或恢复会话时先读这份文件，再核对 `git status` 与实际源码。
> 计数以 `implementation-progress.json` 为准：`python scripts/audit_progress.py counts`。

## 当前状态（最后更新：2026-11-12 UTC+08，UI-02.3 验收后）

- 分支 `main`，跟踪 `origin/main`；最新提交 `8eb35d5`（UI-02.3 代码）
- 已验收：任务 **3/61**（UI-01、UI-02、C13）；切片 **8/148**
  （QA-03.1、UI-01.1、UI-02.1、UI-02.2、UI-02.3、C13.1、C13.2、C13.3）
- 进行中父任务：QA-03（剩 3.2 硬编码 SC 输出路径、3.3 端口冲突与名义断言）
- 基线（当前真实值）：app 单测 **138 passed / 0 failed / 0 ignored / 0 filtered**；
  GUI `ui_regression` **8 passed / 0 failed / 9 ignored**；sheetcore 462 passed / 34 ignored（未改动 core）
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`；`source scripts/audit_env.sh <标签>`

## 已完成（提交 → 证据）

| 切片/任务 | 提交 | 证据 |
|---|---|---|
| QA-03.1 | `d1ebffd` | `qa03-1-*.txt` |
| UI-01（任务级） | `b742b2c` | `ui01-edit-commit.txt` |
| UI-02.1 | `6dc206a` | `ui02-1-*.txt` |
| UI-02.2 | `5535d40` | `ui02-2-*.txt` |
| UI-02.3 + UI-02 父任务 | `8eb35d5` | `ui023-command-registry.txt`、`ui023-suites.txt`、`ui023/*.png`、`ui02-parent-acceptance.txt` |
| C13.1 | `d3a141a` | `c13-1-swallowed-errors.txt` |
| C13.2 | `7129fcd` | `c13-2-suite.txt` |
| C13.3 + C13 父任务 | `e3d2bbf`、`5728dc2` | `c13-3-save-failure-receipt.txt`、`c13-parent-acceptance.txt` |

## 下一步

**QA-03.2 硬编码 SC 输出路径**（父任务 QA-03 的第 2 个切片，P0）

源码位置（qa-requirements.md 指出）：`main.rs` 2896-3015 的 SC_B4/B1/B3 分支写着
`F:/Finance/work/...` 固定输出；`app/tests/ui_regression.rs` 只清理其中三项。

第一条动作：读 `main.rs` 2890-3020，列出所有 `SC_*` 环境变量与其输出路径，去掉固定
开发目录（改为 `AUDIT_RUN_DIR`/临时目录或显式参数），再补一条「设置 SC_B4 但未给路径时
不得写到 F:/Finance/work」的回归。

## 尚未解决的风险 / 未执行

- 断电不可模拟；Office 真实独占锁未构造（只读目标覆盖同一 rename 失败分支）
- 真实 IME、硬件键盘、系统文件对话框、DPI、打印、Excel/WPS 对拍：未执行（QA-04/05/09）
- 窗口关闭时 dirty 文档无保存提示：既有缺口，登记后续
- 网格级 Ctrl+C/X/V/F/H 未进命令表（无 enabled 谓词，留 UI-06/07）
- Supabase 生产数据只读、未接触
