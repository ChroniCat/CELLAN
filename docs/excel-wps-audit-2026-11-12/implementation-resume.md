# 恢复记录

- 分支 `main`（`origin/main`）；代码修复 `5fd9ea3`（本次四项；前序 `f97377a`/`fb2b55d`/`2745438`/`f19b932`/`4202fae`），文档记录提交 `c3711f3`；C11 父项已验收；UI-09 父项仍待真机复核。
- 台账（`python scripts/audit_progress.py counts`）：13 已验收 / 10 待验收 / 3 实施中 / 35 待实施；
  切片 35 已验收 / 27 待验收 / 86 待实施。C11 父项已按用户 14:00 认可翻绿。
- 当前测试真值（15:01 重跑）：core 未改，沿用 **75 bins / 678 passed / 0 failed / 41 ignored**；
  app **217 passed / 0 failed / 0 ignored**，APP_EXIT=0。GUI 快照未重跑，不作当前证据。
- 当前未完成：C22 待验收；UI-05 的 Ribbon 新会话入口已修，真实鼠标/单次查找取消/结果虚拟滚动仍待；
  C10.2 live 按用户要求保留。

## 12:35 当前：11:38 UTF-8 回归修复

- 用户指出 `rewrite_table_column_refs` 用 `text[i..i+table.len()]` 只查总长度，
  `Sales` 长度 5 在 `=[@金额]*[@税率]` 首次 `0..5` 切断“金”字，rename 遍历
  全簿会 panic，无关中文公式也可触发。已改为先检查 `is_char_boundary`、
  再做大小写匹配；新增中文裸当前行公式 + 无关中文公式/字符串回归。
- `table_calculated` 7 passed；全量 core **650/0/41 EXIT=0**、app **201/0 EXIT=0**。
- 用户 11:38 已认可 C11 上轮剩余修复与 C10 生产入口；C11.3/C11.4 和 Office
  实机仍待做，父项 C10/C11 保持实施中，不把本轮当父项验收。

## 12:05：11:13 有限复核的六项整改

- C11.2：新增 `Sheet.spill_shapes` 记录可信锚点/精确形状（1×1 也记录）；
  `materialized_spill` 移除从 `spilled` 邻接尾格猜形状；`A2#` 尾格和旁普通格
  `B1#` 均 `#REF!`，不返回 5；逐表重算仍不重算无关 RAND。`spill_refs` 7 passed。
- C11.1：`cf::shift_formula` 整行分支增加 `bracket_depth`，`SUM(Sales[1:3])`
  下移不被改成 `Sales[2:4]`；save/reload 测试从实际加载公式复制，`Shift.at/n`
  显式非零，不再用硬编码 raw。`whole_row_refs` 7 passed。
- C10.3：转换区域改为目标表限定扫描，Sales 转换不再改 Other 的裸 `[@X]` 和
  `Other[Amount]`；两表结果不变。`table_calculated` 6 passed。
- C10.2：列重命名同步 header 单元格；表名匹配大小写不敏感；字符串不改；
  保存重开表头/列定义/id/公式一致。
- C10.2：`fill_calculated_column` 接入 `TableFillCalculated` 表设计按钮，
  经 preflight/事务/重算；`TableToRange` 也改走 `convert_table_to_range`；
  app 生产入口测试通过。
- 全量 core 650/0/41、app 201/0；原始日志已复制到 c11-whole-row、c11-spill、
  c10-table-calculated、c10-structured 证据目录。上述切片仍待独立复核，父项不翻绿。
- Excel 16.0 与 WPS 12.0 实机：旧缓存保持（B1=10）→ 手动 Calculate（B1=14）→ 另存 .xls/.xlsx 再打开仍手动且 B1=14。
- 旧 core 605/app 196、旧 GUI 12/8 快照均为历史，不再作为当前证据。

## 10:25 复核：C06 已验收（第 12 项）

- 用户认可 `cc3cec5` 的 `ensure_now_snapshot_in(wb.date_system)`；C06 / C06.1 / C06.2
  转已验收，不重做。后续公共公式改动触及日期路径时重新打开。

## 本轮追加：C10.1 原式裸当前行（实现完成，待独立复核）

- 用户指出原样本是 `=[@金额]*[@税率]`（省略表名），此前 `Sales[@Amount]*Sales[@Tax]`
  替代不能关闭。`expand_structured_at` 增加 `cur_col`；裸 `[@...]`/`[[#This Row],...]`
  由 `find_table_at` 按公式单元格行+列定位所属表，同行多表不取错。
- 新增中文原式两行测试、同行双表测试；`structured_references` 12 passed。
- 特殊字符列名仍只覆盖空格/逗号/百分号，括号/结构化转义保留为未覆盖边界。
- 全量 core 71 bins / 636 / 0 / 41；app 200/0。C10.1 仍待独立复核，父项 C10 未完成。

## 历史：C11.2 溢出引用首版（已被 11:45 当前段覆盖）

- 词法 `#` 区分错误字面量与 `A1#`；AST 增加 `Node::Spill`；求值从锚点
  `cell_value` 取得数组，`SUM(A1#)` 随 `SEQUENCE` 扩大/缩小；非数组返回 #REF!。
- Deps 将 spill 引用标为 dynamic，编辑时全量重算；`.xls` 明确拒绝写入 `A1#`。
- `crates/core/tests/spill_refs.rs` 3 项；全量 core 71 bins / 633 / 0 / 41；app 200/0。
- C11.3 隐式交集、C11.4 三维引用尚未实施，C11 父项未验收。

## 历史：C11.1 整行引用首版（已被 11:45 当前段覆盖）

- `normalize_formula` 新增整行展开：`1:3` → `A1:XFD3`、
  `$1:$3` → `A$1:XFD$3`、`Sheet!2:2` → `Sheet!A2:XFD2`；
  跳过引号和结构化引用方括号，依赖与 `refs::rewrite` 共用规范化路径。
- `crates/core/tests/whole_row_refs.rs` 4 项：规范化/字符串、按已用区求值、
  Sheet 作用域、结构化引用不误改、Deps 和插行改写。
- 专项 4/0；全量 core 70 bins / 630 / 0 / 41；app 200/0。
- C11.2 溢出引用、C11.3 隐式交集、C11.4 三维引用尚未实施，C11 父项未验收。

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
- C06 后全量 core：69 bins / 626 passed / 0 failed / 41 ignored / CORE_EXIT=0；
  app 200/0；`c06-local-clock.txt` 与 `c06_core_fix.txt` 是当前日志。
- 09:53 复核回归已修：`ensure_now_snapshot_in(wb.date_system)` 在 D1900/D1904
  实际 `recalc_all_force` / `recalc_sheet` 前按目标体系生成快照；新增回归
  `recalc_binds_now_snapshot_to_workbook_date_system`：注入 2024-02-29 16:30 UTC+08，
  D1900=45352/45352.020833，D1904=43890/43890.020833，同一轮 NOW 差为 0。
- 本机 Excel 16.0/WPS 12.0 空白工作簿对拍：TEXT(60)=1900-02-29、DAY(60)=29，
  NOW/TODAY 用本机 +08:00；证据 implementation-evidence/c06-local-clock.txt。

## 本轮：C10.1 结构化引用语法与求值（实现完成，待独立复核）

- `crates/core/src/table.rs` 新增 `expand_structured_at`：`#Data/#Headers/#Totals/#All`、
  `@`/`#This Row`、列区间、特殊列名；表名可在公式体开头，不再要求 `start > 0`。
- `formula/mod.rs` 顶层 `Node::Range` 返回数组（1×1 仍标量），`=Sales[Amount]` 得到 10/20；
  `formula/deps.rs` 按公式行扫描当前行依赖。
- 审计失败样本恢复：`=Sales[Amount]`、`=SUM(Sales[[#Data],[Amount]])` 均正确；`"Sales[Amount]"`
  不被改写；9 项 `structured_references` 全过。
- 全量：core 69 bins / 625 passed / 0 failed / 41 ignored；app 200/0。
- 未完成：C10.2 计算列/表扩展/列重命名、C10.3 转换区域保存往返；C10 父项未验收。

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

## 13:10 当前：12:48 复核后的删除/UTF-8/兼容链

- HEAD `f1aea10`（C11.3/C11.4/C10 转义/兼容链已提交）；本轮改动已进入该提交，不再以未提交工作树记绿。
- 删除 3D 端点改为按删除前完整 `order` 在区间内部求邻居：
  `First:Third` 删 First -> `Second:Third`；反向 `Third:First` 删 First ->
  `Third:Second`；删 Third -> `Second:First`；`Second:Second` 删唯一成员 ->
  整段 `#REF!`，不会生成 `Third:First`。
- `retarget_3d_endpoint` / `rewrite_sheet` 的 UTF-8 扫描按整字符推进；
  `delete_sheet` 的 `=中文!A1`、`='中文'!A1`、中文 3D 端点已补实际删除测试。
- `xls_write` 对 `.xls` 无法表达的 `@`/`#` 等返回带“兼容提示”的错误；
  `xlsx::save_atomic`/核心写盘失败不覆盖目标；模型公式和缓存保持，App
  保持 dirty 并在状态栏显示提示；另存 `.xlsx` 往返仍保留公式/缓存。
- 当前实测：core **74 bins / 674 passed / 0 failed / 41 ignored**，
  CORE_EXIT=0；app **202 passed / 0 failed / 0 ignored**，APP_EXIT=0。
  专项：implicit 3、three_d 16、structured 14、xls_formula 11、page_setup 1+2 ignored。
- 真实 Office：@ B2/B4=20（Excel/WPS）；quoted 3D=60；Excel/WPS 删 First
  后 50；Excel reverse-end=50、unique=#REF!；xls 改 First A1=100 后 60→150
  （Close(false)，不是 Office 保存往返）。
- 台账：C11.1/C11.2 已验收；C11.3/C11.4 待验收；切片 33 已验收 / 20 待验收 /
  95 待实施；C10/C11 父项仍未验收。证据新增 c11-implicit-intersection、
  c11-three-d-refs、c11-xls-compat、c10-structured-escapes。
- 未完成：完整反向/唯一/中文 Office 交叉矩阵、WPS 全函数矩阵、xls Office
  保存往返、C10.2/C10.3 与 C11 父项独立复核，均不能记绿。


## 13:35 当前：反向真实顺序 + C11 输入/重算保护

- 用户 13:14 指出旧反向测试/Excel 脚本是 `Third,Second,First`，`Third:First`
  实际 `pa0<pz2`，并未覆盖 `pa>pz`。已把专项改成真实顺序
  `First,Second,Third,Other`，公式 `=SUM(Third:First!A1)`，断言 `pa>pz`；
  删 First 得 `=SUM(Third:Second!A1)=50`，删 Third 得
  `=SUM(Second:First!A1)=30`。`three_d` 16 passed。
- Excel 16.0 真实脚本 `c114_reverse_real.ps1`：输入 `Third:First` 后 COM
  归一化为 `First:Third` 60；删 First → `Second:Third` 50，删 Third →
  `First:Second` 30；证据保留“输入/COM 返回值”区别，不冒充引擎反向原文。
- XLS 导出兼容拒绝保护已认可为**导出安全网**，标题改为“XLS 导出兼容保护”；
  它不是 C11 原始输入/重算保护，父项 C11 保持待办。
- 补 C11 原始输入/重算链：选择 **外部工作簿三维引用**
  `=SUM([1]First:Third!A1)`（C11 合法、引擎确实未支持）。Excel 生成夹具
  `crates/core/tests/fixtures/c11_external_3d.xlsx`；`load_diag` 给非致命
  “兼容提示”，打开后 60 保留；`recalc_all_force` 不覆盖缓存；保存重载仍
  公式/60/提示。真正的 `#REF!`/`#DIV/0!`/`#NAME?` 和未知函数仍按错误写回，
  不扩大到所有 parse 失败。`c11_compat` 3 passed；App 真实导入夹具测试通过。
- 当前全量：core **75 bins / 678 passed / 0 failed / 41 ignored**；app
  **204 passed / 0 failed / 0 ignored**。App 204 是新增兼容测试后的当前值，
  不把旧 202/674 或未完成的后台进程当最终绿。
- C11.3/C11.4 仍是“待验收”；C11 父项不翻绿。


## 13:45 当前：兼容检测跳过字符串

- 用户 13:38 指出 `unsupported_reference_combo` 不跳双引号字符串，
  `="[1]First:Third!A1"` 与 `=IF(TRUE,1/0,"[1]First:Third!A1")` 会被误判；
  已改扫描器识别 `"` 字符串及 `""` 转义，字符串内的文本不再触发兼容缓存。
- `c11_compat` 新增真实 recalc 负例：前者缓存 7 -> `Text("[1]First:Third!A1")`，
  后者缓存 7 -> `Error(Div0)`；检测器三条字符串/转义断言均为 `None`。
- 专项 **4 passed**；全量 core **75 bins / 678 passed / 0 failed / 41 ignored**，
  CORE_EXIT=0；app **205 passed / 0 failed / 0 ignored**，APP_EXIT=0。
- 未重做 Office 外部三维夹具，原 60 缓存链仍有效；父项 C11 保持待验收。
- 已提交并推送 `4d1f925`（字符串边界修复）；远端 `origin/main` 与本地一致。


## 13:55 当前：UI-05 多选区查找范围

- 补齐 `extra_sel` 多选区并入查找/全部替换；`session_rects` 锁定主选区与
  Ctrl 多选块，`find_next` 清空当前 `extra_sel` 后仍保留搜索快照。
- 新测试 `find_selection_scope_includes_ctrl_multi_ranges_and_keeps_snapshot`
  通过；`find_` **8 passed**；App 全量 **205 passed / 0 failed / 0 ignored**，
  APP_EXIT=0。core 未改，沿用 **75 bins / 678 / 0 / 41**。
- UI-05 的剩余真实鼠标对话框、单次查找可取消、结果虚拟滚动仍未做，
  UI-05.4/UI-05 保持待验收。

## 14:07 当前：C11 父项验收 + Ribbon 新查找会话 + C22 对齐

- 用户 14:00 监督：结合 C11.1/C11.2、C11.3/C11.4 四切片及真实 Office 夹具/缓存提示链，
  C11 父项转「已验收」，独立接受第 13 项；不再要求 WPS 全函数或每格式 Office 另存矩阵。
  C11.3/C11.4 一并转已验收；旧的“.3/.4 待矩阵”记录清理，保留 xls 仅导出后可重算、
  .xls 拒绝是导出安全网、外部三维组合范围有限三条边界。
- UI-05：新增 `find_open_new_session()`，Ribbon FindReplace、Ctrl+F/H、标题栏搜索框、
  SC_OPEN_DIALOG=find 统一清 `session_rect`/`session_rects`；连续 `find_next` 不走这里。
  新测试 `ribbon_find_replace_clears_stale_scope_before_replace_all`：旧 A1/A3 快照 +
  新 B1/B3 多选 → 真实 `ribbon_action("FindReplace")` → 全部替换只改 B1/B3，
  旧 A 不变，一次 `do_undo(false)` 恢复 B1/B3；专项 1 passed。
- C22：绘制改为 `cell_text_align`，General 文本左、数字右、布尔/错误居中，显式对齐覆盖；
  专项 1 passed。合成工作簿 SC_SHOT 实测 A1 左、B1 右、C1 中、D1 中、E1/F1/G1 显式
  左/中/右；长文本仍按用户偏好默认裁剪，长文本多情况与 Excel 同位置像素对拍未做，C22 待验收。
- 当前全量：app **212 passed / 0 failed / 0 ignored**，APP_EXIT=0；core 未改，沿用
  **75 bins / 678 passed / 0 failed / 41 ignored**。
- 台账：13 已验收 / 8 待验收 / 1 实施中 / 39 待实施；切片 35/19/0/94。
- 已提交：`e81c221`（`main.rs`、`edit_regression_tests.rs`、`ribbon/actions.rs`、progress JSON、
  resume、c11-final-acceptance、ui05-ribbon-session、c22-general-alignment）。
- 下一步：提交/推送 UI-10，然后继续原 61/148（UI-05 真实鼠标/取消/虚拟滚动、C22 长文本、
  其余 P0/P1 任务）。

## 14:13 当前：UI-10 查找页签与关闭覆盖

- Ctrl+H 设置 find.tab=1，Ctrl+F 设置 0；仍走 find_open_new_session 清新快照。
- 查找对话框“关闭”改为局部 close 意图，末尾 open && !close；名称管理器与加载问题窗口
  的同类末尾覆盖一并修复。
- 新测试 ctrl_h_opens_replace_tab_and_ctrl_f_returns_to_find_tab 通过；全量 app
  **210 passed / 0 failed / 0 ignored**。真实鼠标关闭/×/Esc 与焦点回落未做，UI-10 待验收。

## 14:21 当前：UI-18 筛选入口语义

- clear_filter_criteria 保留 filter/箭头，只清条件并恢复条件隐藏行；disable_filter 才移除
  筛选器；reapply_filter 单独重算；hidden_rows_user 手动隐藏行不被清。
- Ribbon FilterClear/FilterReapply、数据菜单、右键菜单统一到三个方法；右键“清除筛选”
  不再误删筛选器，“重新应用”不再误关筛选。
- 专项 2 passed（含 xlsx save/load 后箭头在、条件空、手动隐藏行保留）；全量 app
  **210 passed / 0 failed / 0 ignored**。真实 Excel/WPS 目视与鼠标入口未做，UI-18 待验收。

## 14:25 当前：UI-09.2 视口翻页与切表

- PageUp/Down 改为按 visible_page_rows（真实行高、当前滚动、跳过隐藏行）翻页，
  步长=可见行数-1，保留一行重叠；到 1048575 不越界。
- Ctrl+PageDown/PageUp 调用 switch_sheet_relative，跳过隐藏表并循环；切表不改单元格。
- 专项 page_keys_follow_viewport_and_ctrl_page_switches_sheet 1 passed；全量 app
  **211 passed / 0 failed / 0 ignored**。全量曾因 UI-18 save 测试使用 TEMP 与并行
  test_env 冲突而失败一次，已改 current_exe().parent() 后全绿。
- UI-09.2 待真实 720/1080 键盘与滚动重叠目视；UI-09 父项未验收。

## 14:28 当前：UI-09.1 常用选择/格式快捷键

- Ctrl+B/I/U 接既有 Ribbon 样式命令；Ctrl+A 第一次当前区域、第二次全表；
  Ctrl+Space 整列、Shift+Space 整行；F5/Ctrl+G 名称框焦点请求。
- 专项 common_selection_and_format_shortcuts_work 1 passed；全量 app
  **212 passed / 0 failed / 0 ignored**。F4、Shift+F10、F6 与名称框真实焦点
  仍未做，UI-09.1/09.2 均为待验收。

## 14:30 当前：UI-10 重开保留输入

- 移除 Ctrl+H 分支中 `self.find.replace.clear()`；find_open_new_session 只清明细快照。
- 回归先存 alpha/beta，关闭后 Ctrl+H：open=true、tab=1、两输入保留；关闭 Ctrl+F：
  tab=0、输入保留；再关闭 Ctrl+H 仍保留。
- 专项 1 passed；全量 app **212 passed / 0 failed / 0 ignored**。真实关闭/×/Esc 与焦点
  回落仍未做，UI-10 保持待验收。
- 已按实际系统时间修正 ui09-page-viewport (14:25)、ui09-shortcuts (14:28)、
  ui18-filter-clear (14:21)、ui10 (14:13/14:30) 的时间标注。

## 14:37 当前：UI-09 三处缺口修复

- page_move 同步 scroll：PageDown 把新首行放到视口顶部，PageUp 放到视口底部；
  测试用独立可见行窗口断言首/末行与 scroll，旧实现 scroll=2 的缺陷已被检出。
- next_visible_row 边界隐藏时停最近可见行；首端 0/1、末端 1048574/1048575
  连续隐藏回归通过。
- Ctrl+A / Ctrl+Space / Shift+Space 按选择跨度选取并清 extra_sel：A1:B2 -> 1:2 行，
  C3:D5 -> C:D 列，远端旧多选块不再残留。
- 专项两 test 各 1 passed；全量 app **212 passed / 0 failed / 0 ignored**。
  UI-09.1/09.2 仍待验收，等真机键盘/名称框焦点与滚轮视觉。

## 14:44 当前：UI-09.3 区域内 Tab/Enter

- 新增 nav_rect 保留矩形选区；Tab/Enter 在区域内循环，Shift 反向，跳过隐藏行/列。
- 方向键也按可见行/列推进；单格 Tab/Enter 在 XFD/1048576 不越界；commit_edit
  advance 复用同一推进逻辑。
- 专项 `tab_enter_cycle_inside_selection_and_stop_at_sheet_edges` 1 passed；全量
  app **213 passed / 0 failed / 0 ignored**。UI-09.1/09.2/09.3 均待验收，UI-09
  父项转实施中，未标已验收。

## 14:47 当前：UI-08.1 边框方向

- 新增 BorderSide 命名下标；四方向 Ribbon、InsideH/V、border_outer 全部修正为
  左/右/上/下 = 0/1/2/3，不再裸写反向下标。
- 专项 `ribbon_border_commands_follow_left_right_top_bottom_order` 1 passed；
  全量 app **214 passed / 0 failed / 0 ignored**。格式对话框混合样式（UI-08.2）
  与未改属性保留（08.3）未做，UI-08 父项保持实施中。

## 14:52 当前：UI-08.2 格式对话框

- 新增 fmt_orig；apply_format_dialog 按字段 diff，只提交实际改动；直接确定不重写。
- 混合两格只改数字格式后，字体/字号/颜色/粗斜体各自保留；边框只在开关变化时整组提交。
- 专项 `format_dialog_preserves_unchanged_fields_and_mixed_cell_styles` 1 passed；
  全量 app **215 passed / 0 failed / 0 ignored**。FmtMixed 已让字体/字号/字形/颜色/填充/
  对齐/方向/换行/缩进/数字格式/四边分别显示“混合/？”；真实鼠标/Office 目视和
  UI-08.3 事务/撤销仍未完成，UI-08 父项保持实施中。

## 15:01 当前：UI-09/UI-08 独立审查四项

- nav_rect：Enter A1:B2 改为 A1→A2→B1→B2→A1，Shift 反向；隐藏行列跳过。
- 查找跳转/查找全部/撤销重做/park/activate/reset_editor_state 清 nav_rect；
  sel_rect 校验活动格，补导航生命周期回归。
- page_move 按旧视口首/末可见行翻页，活动格保持页内可见偏移；中部/底部/PageUp
  用独立窗口断言，修正原“新活动格当视口首行”的跳行问题。
- UI-08.3 按 side dirty 只写改动边，左粗红/右双蓝在只加下框后保留 style/color。
- 专项：nav 1 passed、tab 1 passed、page 1 passed、border 1 passed；全量 app
  **217 passed / 0 failed / 0 ignored**。以上均待验收，真实 GUI/Office 仍缺。
