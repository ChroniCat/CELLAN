# 恢复记录

- 分支 `main`（`origin/main`），HEAD `eccadf6`（UI-03 已推送）+ 本轮未提交改动。
- 台账（`scripts/audit_progress.py counts`）：9 已验收 / 5 待验收 / 47 待实施；
  切片 26 已验收 / 7 待验收 / 115 待实施。
- 当前测试真值（本轮源码，round33/34）：core 67 bins / 590 passed / 0 failed / 41 ignored；
  app 159 passed / 0 failed；GUI 12 passed / 0 failed / 8 ignored。
- 专项：`c14_guard` 14 passed（含真实改外链目标回归）；`csv_import` 10 passed；
  app 的 `csv_import_choices_persist_and_refresh_reuses_them`、
  `refresh_is_one_undo_step_that_restores_values_rect_and_source_size` 各 1 passed。

## 本轮：C09/C14 收尾 + UI-04 清除命令（C09/C14/C04 已验收；UI-04 待验收）

- C09：证据文档重写（当前结论在前，首版 RED 标注历史），台账改已验收；
  明确未做系统键鼠点选，round33 日志汇总与进程退出码分开记录。
- C14：补齐三项原登记边界：
  1) 真实 Excel 16.0 保存的 BIFF 夹具（tests/fixtures/real_excel.xls，SHA 7d5213…），
     LAST_LOAD_KIND 断言走 calamine、不跑 OOXML；
  2) SC_SIDE_PANIC 旁路线程 panic → fatal 诊断 + incomplete + 拒绝覆盖 + SHA 不变；
  3) SC_LOAD_WORKER_PANIC 主线程 panic → app catch_unwind → Failed 状态 + 原文件不动。
  c14_guard 16 passed；M1/M2 变异均被抓。台账改已验收。
- C04：按 04:49 复核意见整理台账为已验收，不再挂过期待办。
- UI-04：ClearPart::{Contents,Formats,All} + clear_rects 统一事务/重算/广播/撤销；
  Ribbon 清除菜单三项分别接 clear_all / clear_formats / clear_values；
  Delete 与右键走同一命令；deps 空时先构建再重算。
  3 个新用例 + 3 个变异全过；app 159 passed；GUI 12/8。
- 未执行：Ribbon/右键真实键鼠点击、第二客户端广播观察、关闭窗口 dirty 提示流程。

## 本轮：UI-03 统一写入门禁（待验收）

- `write_preflight` 增加 `write_preflight_on(sheet, keys, what)`；修 `lock_holder_on` 用错 `self.sheet_idx`（跨表取锁持有人）。
- 新增门禁：`clear_values`、`clear_selection`（keys 去重）、`apply_style_xy`（覆盖格式/颜色/填充/对话框）、`do_sort`、`paste_special_apply`。
- `clip_paste` 剪切源改为 `write_preflight_on(source, src_keys)`，受保护源表不再被清空。
- 新增 3 个回归：受保护下清除按钮/多选/格式/快速排序；单格被锁多选整批拒绝；受保护表剪切粘贴整批拒绝。
- 变异：分别去掉格式/排序/clear_selection 门禁，用例均转红；还原后全绿。
- app 全量 155 passed / 0 failed；GUI 12 / 0 / 8。
- 未执行：真实 GUI 逐入口键鼠点验与协作对端广播观察（登记待验收）。

## 本轮：C09 与 C14 复核整改（04:08/04:29）

### C09（待验收）
- `csv::parse_strict_q`/`records_strict_q`：未闭合引号、闭引号后多余字符报错；导入/刷新在写入前解析，失败旧区域不动。
- `decode(Some(enc))` 指定编码失败不再回退 GB18030（返回 had_errors）；自动探测仍 BOM→UTF-8→GB18030。
- 首次导入回写实际编码；`CsvOpts` 增加 `quote` 并持久化（名称文本第 10 段 `q<码点>`）。
- 短行刷新清空新矩形内缺失列；刷新以整表快照+源尺寸入一步 undo，undo/redo 一并回滚 rows/cols。
- 导入对话框（`open_csv_dialog`/`csv_dlg_ui`/`import_csv_choice`）：编码、引号、表头、按列文本 + 真实预览；保存重开后 refresh 复用配置。
- 未执行：Excel/WPS 编码向导三方对拍。

### C14（待验收）
- `notes::clear_sheet_notes` 重建关系时按 `root.namespaces()` 补回 xmlns；External 的 Target 不做包内路径解析。
- `capture_external_rels`/`restore_external_rels` 收窄：仅在目标被引擎归一（https://→https:/）时恢复原文；用户改了目标就保留新 URL，仅补 TargetMode。
- 新增 `editing_an_existing_external_link_persists_the_new_target`：old→new 保存回读 new，断言无 old、TargetMode/命名空间在。
- 未执行：worker panic / sidefail / 正常 .xls calamine 成功路径夹具。

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
- round18：中途有一次多余括号（编译不过），日志无效，不作为结果
- round19（当前代码）：core 560/0/40、app 143、GUI 12/8，全部 EXIT=0

### C14 部分加载失败不得伪装完整工作簿（P0，待验收）
- 新增 `LoadDiag`/`LoadProblem` + `load_diag()`（`load()` 保持兼容）；
  单元格/公式/样式/整表/图表读取失败 = 致命，稀疏回退 = 非致命。
- `Workbook.load_incomplete`：不完整的簿子不拿原文件当保存基准（坏 XML 会让 umya panic）。
- 应用层：`OpenMsg::Loaded` 带诊断；有致命问题时 `save(false)` 拒绝就地保存并说明原因，
  「另存为」允许并解除限制。
- 测试：`core/tests/load_diag.rs` **6 passed**；app `incomplete_load_blocks…` **1 passed**。
- 变异：M1（致命标记改 false）2 failed；M3（去掉基准跳过）app 用例失败；
  M2（缺表兜底检查）**未被抓住**，已登记为未覆盖。
- round19 全量：core 64 bins / **560 passed / 0 failed / 40 ignored**；
  app **143 passed**；GUI **12 passed / 8 ignored**；全部 EXIT=0。

## 下一步
1. 提交 C14（代码 + 证据 + 台账）并推送。
2. 下一个可执行任务：C05（P1，依赖 C01 已验收）空值/布尔/文本比较语义，
   或 C06（P1，无依赖）本地时间与序列 60。
3. UI-01 真实 IME/键盘/Alt+Enter 取证仍阻塞在硬件条件；C09 依赖 C13+UI-03，
   C10/C11 依赖 C03，C12 依赖 C01+C13，C15 依赖 UI-03。

## 风险 / 注意
- Excel COM 必须用反斜杠原生路径；`F:/...` 会 0x800A03EC。
- `Workbook.CustomDocumentProperties` 在本机不可用（对任何文件都返回 0），
  不能用它判定自定义属性可见性。
- 后续建议（不在本轮范围）：我们写出的 `xl/styles.xml` 没有 `<cellStyles>` 默认样式，
  openpyxl 会告警；Excel 能开但值得补。

## 03:26 复核轮（HEAD 176c088）

- C08：VT 命名空间改按解析出的声明集合判断；新增单引号声明夹具（断言 VT URI 只出现一次）；
  `custom_props` 10 passed / 2 ignored；变异回退会被抓。
- C05：`FValue::compare` 空引用按对方类型强制、类别序 数字<文本<布尔；`comparison_semantics` 4 passed / 1 ignored。
- C14：挂起标签保存完成清 `load_diag`/`load_incomplete`；抢救副本 `fresh_base` 全量标脏
  （布局/隐藏/保护/页设置/空白填充格）；批注删空清 comments/VML 部件；图表内部错误传播
  （drawing→缺失 chart 报致命）。`c14_guard` 8 passed、app 4 passed。
- round21 全量：core 66 bins 574/0/41、app 146/0、gui 12/0/8（首轮 ribbon 分组抖动，单跑与复跑均过）。
- 未执行：真实 Excel 另存 .xls 的 calamine 成功分支、工作线程/旁路失败注入、Excel 属性对话框目视。

## 03:49 复核轮（C14 round3 + C09）

- C14：`clear_sheet_notes` 改用 roxmltree 按节点删关系，重建后严格解析再写盘；
  实测坏件 `work/hermes-round21-.../notes.xlsx` 根标签被删已复现。新增 2 项关系 XML 用例
  （首个关系是批注 / 批注与其他关系共存），断言根、剩余关系、Id 列表。
- C14：图表三处吞错改为报错（缺 drawing rels / 缺 rId / chart XML 截断），加 3 个窄夹具。
  `c14_guard` 13 passed；变异 M7/M8/M9 全部被抓。
- C09：CSV 统一解码（BOM/UTF-16/GB18030）+ 记录解析 + 列类型（文本列保留 00123/18 位）
  + 配置持久化 + 原子写入；`csv_import` 6 passed、app 入口 1 passed；
  变异退回 `lines()+split_line` 得到 (3,2) 症状被抓。
- round24 全量：core 585/0/41、app 147/0、gui 12/0/8。
- 待补：编码/列类型预览对话框、Excel/WPS 三方对拍；C14 的工作线程注入、calamine 成功路径、Excel 加密包。
