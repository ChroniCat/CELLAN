# 恢复记录

## 2026-11-14 01:1x 当前检查点

- 分支 `main`，HEAD `d6ff7c9` 已 push；本批另含 UI-05 与恢复记录更新（见下一条提交）。
- 台账：父任务 **21/61 已验收**（新增 C07、UI-01、UI-03、UI-04、UI-05）。
  剩余 40：QA-01, UI-06, UI-20, UI-27, C05, C10, C12, C15, C16, C17, C18, C22,
  QA-04..QA-09, UI-07..UI-09, UI-11..UI-17, UI-19, UI-26, UI-28..UI-30,
  C19, C20, C21, UI-22..UI-25。
- 最近闭环（每条都有证据文件在 `implementation-evidence/`）：
  - C07.1 动态列表/括号表名/按目标格平移（`c07-dv-dynamic.txt`，Excel COM 对拍）
  - C07.2 信息级确定/取消、Esc 不写（`c07-dv-styles.txt`）
  - C07.3 键盘下拉 + 选择后保存（`c07-dv-keyboard.txt`）
  - UI-01 编辑提交链路（`ui01-edit-chain.txt`）
  - UI-04 清除统一命令（`ui04-clear.txt`）
  - UI-03 写入门禁矩阵（`ui03-preflight.txt`）
  - UI-05 查找替换（`ui05-find-replace.txt`）
- 测试基线（最近实测）：
  - `cargo test -p sheetcore --offline -j 2 -- --test-threads=2` → 80 bins / 735 passed / 0 failed / 9 ignored
  - `cargo test -p app --offline -j 2 --bin app -- --test-threads=2` → 252 passed（UI-03 后）
  - `cargo test -p app --offline -j 2 --test ui_regression -- --test-threads=2` → 20 passed
- 构建：`cargo build -p app --offline -j 2 --no-default-features --features glow` → EXIT=0。
- 本机 GUI 取证方式：断开的 RDP 会话，系统级按键/截图不可用；
  用 `SC_SEL`/`SC_SEL2`（选区）、`SC_TYPE`（逐帧 Text）、`SC_KEYSEQ`（逐帧按键）、
  `SC_DV_TRY`（数据验证提交）、`SC_DUMP`（绘制文本+选区）、`SC_SHOT`（framebuffer PNG）。
- 工作树：本批只动 `crates/app/src`、`docs/.../implementation-progress.json`、
  证据目录；无临时文件入库。
- 下一步候选（按 P0 依赖）：UI-06 选择性粘贴语义 → UI-27 合并/格式刷 →
  UI-20 对象内存事务；再往 P1 推 C05/C22/C10.2。
- 注意：不要重开已验收 21 项；不要降级验收标准；单 cargo 进程、`-j 2`、`--test-threads=2`。
- 归档：旧的 596 行历史已另存 `implementation-resume-archive-20261114.md`。
