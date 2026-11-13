# 恢复记录

- 分支 `main`（`origin/main`），HEAD `ae36de3`（C06 代码与证据；C08 alert 普通 Open 证据）。
- 台账（`scripts/audit_progress.py counts`）：11 已验收 / 8 待验收 / 42 待实施；
  切片 28 已验收 / 18 待验收 / 102 待实施。
- 当前测试真值（C06 修改后，09:39 重跑）：core 68 bins / 616 passed / 0 failed / 41 ignored，
  CORE_EXIT=0；app 200 passed / 0 failed / APP_EXIT=0；GUI 12 passed / 0 failed / 8 ignored /
  GUI_EXIT=0。app/GUI 与 C06 修改后的 core 同树运行。
- Excel 16.0 与 WPS 12.0 实机：旧缓存保持（B1=10）→ 手动 Calculate（B1=14）→ 另存 .xls/.xlsx 再打开仍手动且 B1=14。
- 旧 core 605/app 196、旧 GUI 12/8 快照均为历史，不再作为当前证据。

## 本轮：UI-21 手动计算与工作表计算（已验收）

- 三个 07:24 复核风险已补代码/用例：spill、逐表 pending、calcPr 前缀；本轮未回退。
- BIFF CALCMODE 修正（564b979）：旧实现写 Workbook 全局流，内部往返能读但 Excel 16.0/WPS 12.0
  都按自动模式打开；现由每个 sheet_stream 写在工作表 BOF 之后。xls_read 扫描记录流，
  测试增加“CALCMODE 位于第一张工作表 BOF 之后”断言；该断言不声称逐表紧邻。
- 自动测试（当前源码）：core 612/0/41 CORE_EXIT0；app 200/0 APP_EXIT0；GUI 12/0/8 GUI_EXIT0。
- 实际 GUI：真实 app 窗口打开 work/ui21_gui/ui21_gui.xlsx（窗口标题 ui21_gui.xlsx；
  与 fullCalcOnLoad 不同的 ui21_mode_fixture.xlsx 不是本轮 GUI 启动文件），A1 5→7 后 B1 保持 10 且待计算；
  VK_F9 后 B1=14、B2=8，状态仍手动且无待计算。截图与说明在
  implementation-evidence/ui21-gui/ 与 ui21-manual-calc.txt；键盘为 Win32 消息投递，
  因本会话无前台桌面，不是物理键盘。
- Excel/WPS 值/模式闭环（模式识别已实测）：ui21_stale_manual.xlsx（A1=7、B1 故意旧缓存 10）
  在 Excel 16.0 / WPS 12.0 各自同一个 Application 实例中打开均为手动且 B1=10；
  CalculateFull 后 B1=14；同一实例内另存 .xls/.xlsx 再打开仍手动且 B1=14。
  app_saved_manual.xlsx / app_saved_manual2.xls（A1=5，真实保存代码，非 SaveAs 对话框）
  也均为手动、B1=10；它们只核对模式识别，与 A1=7 的陈旧缓存夹具不是同一文件。
- 证据：implementation-evidence/ui21-manual-calc.txt、ui21-gui/{excel-cycle.txt,wps-cycle.txt,
  gui-initial-cached-10.png,gui-after-edit-pending.png,gui-after-f9.png}。
- 未执行边界：“计算工作表”GUI 点选未单独取证（命令级 app 测试覆盖）；大表数组/循环引用
  对拍未执行；.xls 动态数组不在当前 BIFF 模型。UI-21 父项与 UI-21.1 均已验收。

## 本轮：C06 本地时间与序列 60 兼容（实现完成，待独立复核）

- `date_to_serial_in`/`serial_to_date_in` 显式表示 1900-02-29 兼容日：59/60/61 →
  02-28/02-29/03-01，DATE(1900,2,29)=60；1904 体系不伪造该日。
- `with_clock_at(unix_secs, offset_seconds, f)` 注入时钟；NOW/TODAY 用本地偏移；
  `Calc` 生命周期和 `recalc_all_force`/`recalc_sheet` 多遍重算共享 NOW 快照。
- 测试 4 项：序列 59/60/61 与 TEXT/DAY/MONTH/YEAR/单元格格式；注入
  Asia/Shanghai 00:30（UTC 前一天 16:30）；时区变化和午夜；同轮 NOW-NOW=0。
- C06 后全量 core：68 bins / 616 passed / 0 failed / 41 ignored / CORE_EXIT=0。
- 本机 Excel 16.0/WPS 12.0 空白工作簿对拍：TEXT(60)=1900-02-29、DAY(60)=29，
  NOW/TODAY 用本机 +08:00；证据 implementation-evidence/c06-local-clock.txt。

## 本轮：C08 无损更新自定义文档属性（已验收）

- 实现已在 `external.rs`/`xlsx.rs`：按命名空间保留第三方属性、只重写 `_sc_src_*`；
  四类属性与 `&<>"` 转义、删最后源只清本应用属性等 10 个测试通过（2 个需 Office 的 ignored）。
- 实机补证：app 真实 `SC_SAVE_ASYNC` 保存 input.xlsx → app_saved.xlsx，仍含 5 项；
  Excel 16.0 cscript 读取 5 项；Excel 另存后仍 5 项；WPS 12.0 读取 5 项，另存后
  5 项原样并追加 WPS 自有 3 项。PowerShell 5.1 动态 COM 的 NullReferenceException
  不是属性不可见，cscript late binding 正常。
- 最终边界：09:26 独立复核指出旧 `DisplayAlerts=False` 不能证明“无修复提示”；
  09:34 用 app_saved.xlsx 原字节副本（SHA-256 c42e79…eb48）在 Excel/WPS
  `DisplayAlerts=True、Visible=True` 的普通 `Workbooks.Open` 打开，均
  177ms/666ms 返回，只有文件窗口，无修复/恢复对话框，打开前后哈希不变。
  属性页列表截图仍空白，但复核已明确不是原文门槛；刷新只验证模型层 import_csv，
  复核明确不要求补 UI 刷新。
- 日期口径：XML 的 `Reviewed=2024-03-01T08:30:00Z`（UTC）在本机 COM 显示为
  `2024-03-01 16:30`（+08:00），只是同一时刻的显示差异；三份文件 XML 文本未变。
- 证据：implementation-evidence/c08-custom-props.txt、c08-custom-props/
  （含 c08_open_alerts_excel.txt / c08_open_alerts_wps.txt）。

## 本轮：UI-27 合并/格式对话框/格式刷（待验收）

- 合并有内容损失时先提示，二次确认才执行；确认绑定 wb_epoch/doc_gen/表/矩形/锚点/待删内容快照，
  换表、换文档、改内容都重新提示；Esc 与切换标签/工作表会清挂起。
- MergeEdit 改记 added/removed 差异；undo/redo 只增删差异，保留动作后新增的无关合并；
  相交冲突跳过恢复，避免覆盖协作方后加结构。
- 格式对话框的合并改为挂起，确定才执行；取消/关闭完全不改。
- 格式刷保存源区域样式矩阵 + wb_epoch/sheet；跨文档/跨表校验失败即取消，避免同 sid 套错。
- 合并撤销/重做按 Applied.merge_changes 广播；双端 poll_net 断言 undo/redo 一致且保留 B 后加合并。
- app 192 passed / 0 failed；GUI 12 passed / 0 failed / 8 ignored；core 599/41（68 bins）。
- 变异 M22–M27、M32–M35 全红。未执行：真实 GUI 双击/拖选/Esc、Excel 模态提示对比。

## 本轮：UI-06 选择性粘贴（待验收）

- formula/全部都走 refs::translate（相对平移、$ 锁定、负偏移 #REF!）；
  转置时矩阵与源坐标一起转置，偏移始终用原始源坐标。
- 全部同时写源样式；格式模式只写样式；运算除零写 #DIV/0!，不再静默保留；
  外部文本与普通粘贴共用 parse_clip_text，保留中间空行。
- 7 个新用例：正负偏移、转置源坐标、全部含格式+除零、TSV 空行、外部剪贴板优先、全部公式平移、转置负位移；变异 M17–M21、M28–M31 全红。
- app 188 passed / 0 failed（UI-06 完成时）。
- 未执行：Excel/WPS 实机对拍、真实系统剪贴板跨应用、跨簿样式映射（UI-07）。

## 本轮：UI-05.4 选区/列序/查找全部/分帧取消 + 三项安全语义（待验收）

- 范围可选 当前工作表 / 整个工作簿 / 当前选区；顺序可选按行/按列；对话框新增“查找全部”
  结果窗口（点击跳转，最多显示 500 项）。
- 查找全部/全部替换改为候选键预排 + 每帧 4000 格扫描，可取消；小范围（≤8000 格）同步完成。
- 05:48 复核三项修复：
  1) replacement_at 增加 sheet 参数，匹配/显示串/公式都用命中表，不再固定活动表；
     跨表夹具 A!A1=keep、B!A1=old、B!D4=old、B!C3 百分比；
  2) FindState.session_rect 锁定搜索选区，连续 Next/Prev 不再退化成单格回绕；
  3) FindScan.wb_epoch + park/activate/reset 主动取消，跨文档旧扫描不得写新文档；
  4) 附带把 c14 的 LAST_LOAD_KIND 改为线程局部，消除并行测试串台。
- 用例：core 7 passed；app 新增 find_selection_scope / selection_scope_survives /
  long_find_scan / find_all_collects / replace_all_across_sheets /
  find_dialog_and_results / replace_all_uses_the_hit_sheet / stale_scan_is_dropped /
  parking_a_document 共 9 项；变异 M10–M16 全红。
- 未执行：真实键鼠点击对话框/取消/结果项、多选区、单次查找可取消、反向 UI。

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
