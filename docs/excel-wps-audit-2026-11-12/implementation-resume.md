# 恢复记录

## 2026-11-14 02:0x 当前检查点

- 分支 `main`，HEAD `a6a796c` 已 push（本文件随后单独提交）。
- 台账：父任务 **28/61 已验收**。新增本轮：C15（保护细粒度）、C22（对齐与长文本裁剪）、C17（条件格式优先级）。
  剩余 33：QA-01(P0), C10, C12, C16, C18, C19, C20, C21, QA-04..QA-09,
  UI-07..UI-09, UI-11..UI-17, UI-19, UI-22..UI-26, UI-28..UI-30。
- 最近闭环（证据都在 `implementation-evidence/`）：
  - C15 `c15-protection.txt`：Excel 实测「属性存在且为 0 = 允许」；逐格判锁、
    排序/插删行/格式按权限放行；Excel 生成夹具 → 只改未锁 B1 → Excel 复核 denied/ok/ok/denied。
  - C22 `c22-alignment-spill.txt`：`spill_clip` 只往空邻格溢出；本应用 / Excel 16.0 / WPS 12.0
    同区域逐行墨迹分段一致，H4 右邻有内容时在本列边界截断且显示开头。
  - C17 `c17-cf-priority.txt`：cfvo 类型（percent 3 / formula 4 / percentile 5，实测枚举）、
    priority 原样往返、全局按 priority 求值、stopIfTrue 跨范围、Dxf 数字格式/边框、
    x14 扩展警告；Excel 重开保存文件与改无关单元格后颜色/priority/cfvo 均一致。
- 测试基线（最近实测，同一 cargo 串行）：
  - `cargo test -p sheetcore --offline -j 2 -- --test-threads=2` → 83 bins / 750 passed / 0 failed
  - `cargo test -p app --offline -j 2 --bin app -- --test-threads=2` → 262 passed
  - `cargo test -p app --offline -j 2 --test ui_regression -- --test-threads=2` → 20 passed
- 构建：`cargo build -p app --offline -j 2 --no-default-features --features glow` → 0。
- 本机 GUI 取证：RDP 断开会话下系统截图/按键不可用；用 `SC_SEL/SC_SEL2`、`SC_TYPE`、
  `SC_KEYSEQ`、`SC_DV_TRY`、`SC_DUMP`（含 `textclip`/`cf-fill`/`cf-border`）、`SC_SHOT`、
  `SC_SHOT_FRAMES`、`SC_SHOT_EXIT`、`SC_OPEN_DIALOG=ps|dv|protect|cf`、`SC_CMD`、
  `SC_INSERT_PIC`/`SC_PIC_SEL`/`SC_PIC_RECT`。
- Excel/WPS 对拍方式：COM 建夹具 → `Range.CopyPicture` + 剪贴板导 PNG → Pillow 逐格取色/墨迹分段；
  脚本在 `scripts/excel_c17_cf_fixture.ps1`、`excel_c17_cf_verify.ps1`、
  `excel_c22_range_picture.ps1`、`wps_c22_range_picture.ps1`、`excel_c15_protection_fixture.ps1`。
- 工作树：干净；无临时文件入库。
- 下一步候选：C16 打印/PDF 与页面设置（P1，已有页面设置基础）→ C18 样式模型与未知样式保留
  → C19/C21 合并与筛选排序语义 → QA-01 兼容性基线（P0，工作量大，需单独预算）。
