# 恢复记录

- 分支 `main`（`origin/main`），HEAD `dece894`（C04 首版已推送）+ 本轮未提交改动。
- 台账（`scripts/audit_progress.py counts`）：6 已验收 / 3 待验收 / 52 待实施；
  切片 17 已验收 / 5 待验收 / 126 待实施。
  C02 已验收（用户 02:43 复核认可）；C04、C08 待验收（用户复核中）。

## 本轮（2026-11-13 02:45–03:05）：C08 + C04 两条补证

### C08 无损更新自定义文档属性（P0，待验收）
- 根因：`external::write_sources` 每次新造整份 `docProps/custom.xml`，第三方属性全丢；
  调用点还有 `!wb.sources.is_empty()` 前置条件，删最后一个源时不清残留。
- 修法：合并式写入（第三方属性原片段+pid 保留，只替换 `_sc_src_*`；pid 接着最大值分配；
  名称/值转义；无第三方且无源时删部件+关系+Content_Types；从 base 文件读回 umya 已丢弃的部件）。
- 附带修复：`Source.name` 为任意文字（如「导入1」）时归一成 `_sc_src_N` 并保证唯一，
  否则保存后读端认不出、源直接消失（round17 的 sfi 失败由这条触发）。
- 测试：`crates/core/tests/custom_props.rs` **8 passed / 0 failed / 2 ignored**；
  `save_failure_injection` **11 passed / 0 failed / 1 ignored**。
- 变异：整份重写 → 6 failed；不清理 → 3 failed；不转义 → 1 failed（首版 0 failed，已补断言）。
- 实机：Excel 16.0 / WPS 12.0 打开夹具→改格→另存，部件与四类属性原样保留，
  `excel_roundtrip_keeps_custom_props` 两边各 1 passed；openpyxl 读出 5 项属性。
- 未执行：属性对话框目视（本机 COM `CustomDocumentProperties` 对 openpyxl 合法文件也返回 0）。

### C04 两条原要求补证
1. **导出新路径双软件打开**（`date_system::make_c04_export_fixture`，ignored）：
   本项目写夹具 → 本项目导入 → 改 B2=7（非日期）→ 另存 `export_1904/1900.xlsx` →
   Excel 16.0 与 WPS 12.0 各自打开：Date1904 True/False、A1=1 显示 1904-01-02 / 1900-01-01、B2=7。
   产物 `implementation-evidence/c04-probe/export/`（含 probe JSON）。
2. **应用层粘贴链路**（`edit_regression_tests::xmlss_datetime_pastes_…`，1 passed）：
   同一 XMLSS 块 → `convert_block` → `SheetApp::paste_office_block` →
   1904 簿 1.5、1900 簿 1463.5（差 1462），时间小数 0.5 保留，普通数字 1234.5 不变，
   两边格式含 `yyyy…hh`，显示同为 `1904-01-02 12:00`。
   变异：固定 D1900 → 失败（left 1463.5 / right 1.5），已还原。

## 已执行命令与结果
- `cargo test -p sheetcore --test custom_props` → 8 passed / 0 failed / 2 ignored（EXIT 0）
- `cargo test -p sheetcore --test save_failure_injection` → 11 passed / 0 failed / 1 ignored（EXIT 0）
- `cargo test -p app --bin app xmlss_datetime_pastes` → 1 passed（EXIT 0）
- `cargo test -p sheetcore --test date_system -- --ignored make_c04_export_fixture --nocapture` → 1 passed
- `powershell scratch/c04_export_probe.ps1 <dir>` → 4 行读数（Excel/WPS × 1904/1900）全部符合预期
- round17（旧代码）：core FAILED 1（sfi，已修）；app 141 passed；GUI 12 passed / 8 ignored
- round18（当前代码）：运行中，日志 `scratch/round18_{core,app,gui}.log`

## 下一步
1. 读 round18 结果；全绿则把数字写进 resume/台账并提交推送。
2. 下一个可执行任务：C14（P0，无依赖）部分加载失败不得伪装完整工作簿；
   之后 C05（P1，依赖 C01 已验收）、C06（P1，无依赖）。
3. UI-01 真实 IME/键盘/Alt+Enter 取证仍阻塞在硬件条件；C09 依赖 C13+UI-03，
   C10/C11 依赖 C03，C12 依赖 C01+C13，C15 依赖 UI-03。

## 风险 / 注意
- Excel COM 必须用反斜杠原生路径；`F:/...` 会 0x800A03EC。
- `Workbook.CustomDocumentProperties` 在本机不可用（对任何文件都返回 0），
  不能用它判定自定义属性可见性。
- 后续建议（不在本轮范围）：我们写出的 `xl/styles.xml` 没有 `<cellStyles>` 默认样式，
  openpyxl 会告警；Excel 能开但值得补。
