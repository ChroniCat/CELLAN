# 恢复记录

## 2026-11-16 检查点：UI-25 隐藏行列统计口径统一（Excel UIA 实测）、UI-11 公式辅助首批

- 代码 HEAD `61c9b67` 已 push（UI-25 `fac2b22` + UI-11 `61c9b67`，本文件随后单独提交）。
  单 cargo `-j 2`，测试 `--test-threads=2`。
- 台账：**24 已验收 / 61**（数字不变）、27 待验收、3 实施中（QA-01、C12、UI-11）、7 待实施。
- 测试：app `--bin app` **308 passed / 0 failed / 0 ignored**（`work/goal-logs/ui25-app-full.log`）。
- **UI-25（仍待验收）**：最大缺口是统计只看筛选隐藏行，右键「隐藏行/隐藏列」写的
  `hidden_rows_user` / `hidden_cols` 没进统计。用本机 Excel 16.0 状态栏 UIA 实测定口径：
  **隐藏行（手动/筛选/分级折叠）不计，隐藏列仍计入**；计数含文本、数值项只吃数字。
  新增 `stats_hidden_rows()` 作为唯一行过滤来源，删掉「列宽 0 就跳过」的错误判断；
  大选区/普通两条状态栏路径共用同一入口。应用三组实测与 Excel 逐项一致
  （2.3333/4/7、隐藏第 2 行 2.5/3/5、隐藏 B 列 50.5/2/101）。
  证据 `implementation-evidence/ui25-hidden-marker.txt` + `ui25-probe/*.ps1`。
  not_done：真实鼠标右键隐藏未执行、百万行密集选区未压测。
- **UI-11（实施中，未验收）**：`=SU` 出 SUM/SUMIF 候选（↑↓/Tab/Enter）、`=SUM(` 参数签名、
  F4 四态循环只动末尾引用、Alt+Enter 改多行 TextEdit 按光标插入；
  接受候选后显式把光标搬到末尾（否则插进函数名中间）。证据 `ui11-formula-help.txt`。
  - **UI-11.3（本轮，仍实施中）**：编辑公式时点格子 → 光标处建立/替换引用（`insert_reference`），
  鼠标松开帧不提交、选区不动，被引用格描细蓝框（`pick_rect`）；引用按 Excel 四色上色
  （`TextEdit::layouter` + `formula_layout_job`，同一文本同色，`Sheet2!$B$2`/`A1:B2` 整段）；
  Alt+Enter 改成多行 TextEdit 按真实光标插行，并有真实事件用例。提交 `911ad41`，
  app 测试 **313 passed**。GUI 实测 buf=`=A1+B1`、A1 蓝 B1 绿（像素复核）、B1 细框，
  证据 `ui11-ref-pick.txt` + `ui113-probe/`。
  未做：Shift/拖动扩区域引用、行号列标引用、跨表引用、函数向导/最近使用（11.4）、真实 IME。
- 仍未关闭的父项与上一检查点相同；下一优先：C12.4 真 pivotTable、QA-01 父项矩阵、
  UI-11.3/11.4、UI-24 可访问性。

## 2026-11-14 检查点：UI-23 KeyTips/右键键盘，UI-08/09 待验收

- 代码 HEAD `fb2ed65` 已 push（UI-23 实现 `ba1c22c` + 证据提交）。
  单 cargo `-j 2`，测试 `--test-threads=2`。
- 台账：**24 已验收 / 61**，26 待验收，3 实施中（QA-01、C12、UI-17），
  8 待实施（QA-04/05/06/07、UI-11/24/29/30）。24/61 是台账计数，不是监督认可数。
- 测试：app `--bin app` **305 passed / 0 failed**；sheetcore lib **295 passed**；
  日志 `work/goal-logs/ui23-app-final.log` / `ui23-core-final.log`。
- **UI-23**：Alt 页签后进入命令级 KeyTips；开始 B=所有框线、数据 F=筛选、
  视图 F=冻结，Excel 16.0 保存回读分别 Borders=1,1,1,1、AutoFilterMode=True、
  FreezePanes=True/SplitRow=1/SplitColumn=1。Shift+F10 打开当前对象菜单，
  ↑↓/→/Enter/Esc 逐级操作；菜单和子菜单按窗口边缘内移、子菜单不覆盖父菜单。
  证据 `implementation-evidence/ui23-keytips.txt` 与 `ui23/*.png`。
  not_done：物理键盘/鼠标未执行；仅三个命令配置了 KeyTip。
- **UI-08**：四方向/外框/内横线保存重开后 Excel 逐边回读通过；
  格式对话框物理鼠标未执行，父项待验收。
- **UI-09**：720p/1080p 真窗口 PageDown dump 步长 20/40 行，活动格/scroll 同步；
  真实物理键盘/IME 未执行，父项待验收。
- 仍未关闭：C12.4 真 pivotTable、QA-01 父项矩阵、UI-17 父项更新、
  C15/C17 监督复核、UI-19 键盘下拉实证、UI-24 可访问性。
- 下一优先：UI-24 对话框一致性/屏幕阅读；UI-11 公式输入辅助；C12.4。

## 2026-11-14 检查点：UI-07 三入口粘贴与 Excel/WPS 富文本往返

- 代码 HEAD `16c865d` 已 push（UI-07 实现 `537dbe5` + 证据提交）。单 cargo `-j 2`，
  测试 `--test-threads=2`。
- 台账计数仍为 **24 已验收 / 61**；UI-07 全系转 **待验收**（不是监督认可）。
- 测试：sheetcore lib **295 passed**、全部 integration 0 failed；app `--bin app`
  **302 passed**。日志 `work/goal-logs/ui07-final-app.log` / `ui07-final-core.log`。
- **UI-07**：Ribbon/右键/Ctrl+V 共用 `clip_paste`；系统剪贴板比内部 clip 新时优先
  Office 块（XMLSS→HTML），纯文本才退回 TSV。App→Excel/WPS 的 HTML 保留日期/金额
  格式、粗体、填充、合并、单元格内换行；向外公式仍丢失（B2 缓存值 2469，已列明）。
  Excel/WPS→App：XMLSS 保留公式/类型/字体/数字格式；WPS HTML 增加 class 样式、
  数字实体、x:num/x:str、mso-number-format 解析，日期以序列号+格式存回。
  证据 `implementation-evidence/ui07-clipboard.txt` 与 `ui07/*.png`。
  not_done：物理鼠标右键点击/真实键盘 IME 未执行；向外公式保留未达成。
- 仍未关闭：C12.4 真 pivotTable、QA-01 父项矩阵、C15/C17 监督复核、UI-19 键盘下拉实证。
- 下一优先：UI-23 KeyTips/右键键盘操作；UI-24 可访问性；C12.4。

## 2026-11-14 检查点：UI-25 四项缺口、UI-14.3、C15 整块门禁

- 代码 HEAD `8e58657` 已 push；单 cargo `-j 2`，测试 `--test-threads=2`。
- 台账计数 **24 已验收 / 61**。这是台账计数，不是“监督已认可 24 项”；监督仍在逐项复核。
- 测试：sheetcore lib **292 passed**；app `--bin app` **287 passed**（本轮曾出现
  `merges::parses_cols_and_rows`、`xls_write::round_trips_...` 2 个失败，根因是把
  `col_width_to_px` 全局改成 +5 破坏了既有列宽序列化契约；已回退该全局改动，
  未通过改断言掩盖，两个测试恢复真实通过）。
- **UI-25（仍待验收）**：统计覆盖主选区+Ctrl 追加选区；去掉 200k 格硬门槛，大选区
  按 Store 稀疏格分帧、显示「计算中」；跳过筛选/隐藏行列；mask=0 时「统计 ▾」入口
  常驻并可复制结果。证据 `implementation-evidence/ui25-stats.txt`。
  not_done：真实鼠标 Ctrl 多选/右键点选未做；百万行密集选区分帧未压测；
  手动隐藏+筛选隐藏组合未与 Excel 逐项对拍。
- **UI-14.3（待验收）**：行高校准到 Excel（R1 42.75pt、R2 14.25pt）；列宽 43.28 vs
  Excel 41.5（默认字体度量差异，末字符完整，未改换算系数）。
  证据 `implementation-evidence/ui14-autofit.txt`。
- **C15（待验收，等复核）**：拖选/Shift+点击/Shift+方向键/Ctrl+点击全部走
  `rect_sel_blocked` 整块门禁；新增真实指针事件回归。
  证据 `implementation-evidence/c15-selection-gate.txt`。
- **C17**：diagonal 污染 fill 的修复与 load/save/reopen 回归已获独立复核认可，本轮未重做。
- **QA-02**：既有独立 clone 验收已成立，本轮不扩其范围。
- 仍未关闭：C12.4、QA-01 父项矩阵、UI-19 键盘下拉实证、C15/C17 复核结论。

## 2026-11-14 检查点：C12、QA-01.3/08、UI-17/25/28 进待验收

- 代码 HEAD `3c7d102` 已 push；台账 **24 已验收 / 61** 不变，新增只进待验收。
- 测试基线：sheetcore **817 passed**；app `--bin app` **271 passed**。
- 单 cargo：所有 cargo 命令 `-j 2`，测试 `--test-threads=2`。
- **C12.2/C12.3**：多字段透视模型 + 字段面板/刷新；与 Excel 真透视数值对拍。
  值化网格 Excel/WPS 可编辑往返；**多字段真 pivotTable 未实现，C12.4 仍待实施**。
- **QA-01.3**：真 pivotTable 加入三方 runner，chart/page/pivot 9/9 通过。
- **QA-08**：旧审计标注快照边界（90/257 vs 123/257、v1.47 提案），
  新增 revision-traceability.md 作为旧文档与台账的索引。
- **UI-17**：三个切片均待验收——多级排序级别可调（2,4,1,3 回归）、
  指定列去重对话框+实时预览、分列覆盖确认与扩展区域完整撤销（修真实 bug）。
- **UI-25**：状态栏统计 Excel 口径（2.6/10/26 实测），右键六项、掩码持久化。
- **UI-28**：名称管理器支持工作表作用域，局部名称优先；修复 definedName 带 `=`
  导致 Excel 拒开；Excel/WPS 回读 COUNT=3。
- 未关闭：C12.4、QA-01/02 父项矩阵、C15/C17 监督复核、UI-19 键盘下拉实证等。

## 2026-11-14 检查点：C12.2/C12.3、QA-01.3、UI-25 进待验收

- 代码 HEAD `32e9390` 已 push；台账 **已验收 24 / 61** 不变，新增只进待验收。
- 测试基线：sheetcore **815 passed**；app `--bin app` **268 passed**。
- 单 cargo 继续：全部命令 `-j 2`、测试 `--test-threads=2`。
- **C12.2/C12.3（待验收）**：新增 `pivot_multi.rs` 多行/多列/多值/筛选/小计/总计，
  行与列用拼音序；与 Excel 真透视同数据对拍（北京 20/1、华东小计 130/2、总计 150/3），
  `c12_multi` 5 条；透视对话框新增「多字段…」面板，生成/刷新先清旧区域；
  值化网格 Excel/WPS 可编辑往返；**多字段真 pivotTable 部件未实现，C12.4 仍待实施**。
- **QA-01.3（待验收）**：`qa01_extended_roundtrip.py` 增加 `pivot` 类别，
  用 Excel COM 造真 pivotTable 夹具；chart/page/pivot 9/9 三方路径通过，
  JSON 与证据入 `implementation-evidence/qa01*`。
- **UI-25（待验收）**：状态栏统计按 Excel 口径（计数=非空格、数值统计只吃 Number），
  默认平均/计数/求和，右键六项复选，`stat_mask` 持久化 `ui.ini`；
  `fixtures/c10/table.xlsx` A2:E5 实测 `2.6 / 10 / 26`；app 268 passed。
- 仍未关闭：C12.4 多字段真透视、QA-01 父项矩阵、C15/C17 等待监督复核。

## 2026-11-14 检查点：C19/C20/UI-15.3 完成，监督 C15/C17 缺口关闭

- 分支 `main`，代码 HEAD `6d59ed7` 已 push（本文件随后单独提交，代码以该 SHA 为准）。
- 台账（`implementation-progress.json`）：**已验收 24 / 61**、待验收 11、实施中 5、待实施 21。
  已验收数仍是监督认可的 24，本轮新增只进「待验收」，未自行抬高。
- 最近测试基线：sheetcore **810 passed**；app `--bin app` **266 passed**；
  定向 `c19_advanced` 4、`c19_combo` 5、`c19_axes` 7、`c17_dxf_fill` 3、
  `ui15_style` 3、`ui15_fixture` 1、`data_tools` 9。
- 单 cargo 规则继续执行：所有命令 `-j 2`、测试 `--test-threads=2`，
  `.cargo/config.toml build.jobs=2`、`RUST_TEST_THREADS=2` 保留。

### 本轮完成（均待监督复核）

- **C16 打印/PDF**：`display.rs` 共用显示模型、`print::page_ops` 分页/标题/页眉页脚、
  PDF 图片（JPEG/PNG）、图表/文本框进打印；`c16_print` 6、`c16_objects` 3；HEAD `5fbaf94`。
- **C18 样式保真**：主题色/tint（`style_theme.rs`）、图案与四边独立边框/对角线（`style_ext.rs`）、
  quotePrefix 修复（`ensure_quote_prefix`）；Excel/WPS 回读一致。
- **C21 部件保真**：线程批注/persons 按基准字节恢复；queryTable/customXml 原字节保留；
  能力矩阵 `capability-matrix.md`；HEAD `34348ff`。
- **C10 表格**：表头改名同步 `rename_table_column` 与结构化引用；转区域；Excel/WPS 对拍；
  HEAD `568f325`。
- **C19 图表**：C19.2 组合图逐系列类型/次轴（`marker=none` 修正）、C19.1 轴格式/散点真实 X/
  空值占位、C19.3 雷达图 + chartex（瀑布/直方）原图保留与占位；Excel/WPS 类型 81/119/118
  未降级；HEAD `6a339fb` 之后。
- **C20 数据工具**：`collate.rs` 用 Windows zh-CN 排序键做拼音序（Excel 实测
  `陈七,李四,王五,张三,赵六` 一致）；自定义序列排序；分列支持引号/连续分隔符/前导零/
  日期识别/覆盖二次确认/右移一次/重算/撤销；HEAD `4f7168d`。
- **UI-15.3**：`CellStyle.double_underline` / `vertical_text` 独立建模，
  `<u val="double"/>`、`textRotation=255`，绘制两条线与逐字竖排，格式对话框开关；
  Excel/WPS 回读 `UL=-4119 ORI=-4166`；HEAD `6d59ed7`。

### 监督 04:14 三项缺口（本轮已关闭，父项仍待验收）

1. **C15 Enter/Tab**：`advance_active` 现在逐候选格查 `sel_blocked_at`，
   全部禁选时原地不动并提示；回归 `keyboard_advance_skips_cells_without_selection_permission`。
2. **C15 名称框整块**：`rect_sel_blocked(a,b)` 逐格检查（>20 万格按权限位保守判断），
   `A1:B2` 含锁定格整块拒绝；回归 `name_box_range_rejects_when_any_cell_is_forbidden`。
3. **C17 对角颜色污染**：`parse_dxfs` 跟踪 `in_font/in_border/in_fill/skip_color`，
   只有 `<fill>` 内 color 写填充；对角颜色只触发 `dxfs_unsupported` 告警；
   `c17_dxf_fill` 三条含保存重开 `fill=None`。

### 待监督复核清单（不改回已验收）

UI-01、C07、C15、C17、C16、C18、C21、C10、C19、C20、UI-15 均为待验收；
其中 UI-01/C07 的真实键鼠/IME 证据仍缺（RDP 不可用，如实保留缺口）。

### 已知限制/风险

- chartex 高级图种（瀑布/直方/树图/箱线等）只保留原图 + 占位，不能编辑；雷达图无填充与轴刻度。
- 分列只有分隔符模式；固定宽度、逐列格式未做；日期只认 `yyyy-m-d` / `yyyy/m/d`。
- 排序无笔画/部首与按行排序。
- 双下划线只做普通单/双线，会计用 singleAccounting/doubleAccounting 未建模。
- 条件格式未建模方向（对角/内部）保存会丢，已告警；本次只保证不污染填充。
- C07.3 键盘下拉真实写入仍未证实（历史探针 B2=None，仅证明下拉可开）。
- openpyxl 读不到本实现写出的图片，图片验收一律用 Excel COM。
- 无 `SC_SHOT_EXIT` 时 GUI 不自退（历史 RUN=124），截图须配 `SC_SHOT_FRAMES`+`SC_SHOT_EXIT`。

## 下一步

1. C12 透视多字段（C12.2/C12.3/C12.4）：字段面板、多行/多列/多值、两款 Office 可编辑往返。
2. QA-01.3 扩展图表/透视/打印夹具，补齐三方 runner 的 manifest。
3. UI-09 键盘导航剩余切片、UI-16 标签管理复核；UI-15 其余切片收尾。
4. 需要真实输入/IME 的项（UI-01、QA-04、UI-11、UI-24…）等可用桌面会话，不伪造。

## 2026-11-16 检查点二：UI-11.4 插入函数向导 + 真实最近使用

- HEAD `7169cc6`（`feat(ui11): insert-function wizard with search, arg fields and live recent functions`）。
- app 全量：`cargo test -p app --offline -j 2 --bin app -- --test-threads=2` → **320 passed / 0 failed / 0 ignored**（10.71s）。

### 这一轮做完的

1. 插入函数向导（`InsertFunction` / fx）：左列表是引擎真实实现的 475 个函数，可按名字或首字母搜索，
   前缀命中排前；右侧名字/中文说明/签名 + 按签名解析的参数框（`[x]` 标「（可选）」）+ 实时公式预览；
   空参数插 `=NAME()` 且光标停在括号里；已在公式里只插函数体不叠等号；Enter=确定、Esc=取消；搜索框自动聚焦、
   当前函数自动滚进视野、按钮右对齐。
2. 函数库「最近使用的函数」改成运行时读真实记录（`crates/app/src/fnrecent.rs`，
   `%LOCALAPPDATA%\SheetsNative\fnrecent.txt`，MRU 最多 10 条，无记录才兜底 XML 那五项）；
   公式提交时扫描真实函数调用记账，菜单点一下插 `=NAME()`。

### 实测与踩坑

- 对话框第一版把 475 行函数排成一行（ScrollArea 继承了外层 horizontal 布局），窗口宽 48893px 把网格盖白；
  改 `allocate_ui_with_layout(vec2(avail, 330)) + ui.vertical` 后稳定在 620×449。
- 回车确定后同一帧的格内编辑器又把 Enter 当「提交并下移」，A1 被写死、选区跳到 A2；
  修法：确定时 `consume_key(Modifiers::NONE, Enter)`。
- 探针实测：`SC_CMD=InsertFunction` + `SC_TYPE="14:AVERAGE"` + `SC_KEYSEQ="26:enter"` →
  匹配数 475→5、`applied formula="=AVERAGE()" editing caret=9`，编辑栏与格内编辑器同串，状态栏「已插入函数 AVERAGE」。
- 真实用户目录 `fnrecent.txt` 内容：`AVERAGE` / `SUM`（向导确定记一次 + 公式提交再记一次，顺序正确）。

### 证据

- `implementation-evidence/ui11-fn-wizard.txt`
- `implementation-evidence/ui114-probe/{wizard-apply3.png,wizard-apply3.log,wizard9.png,wizard.png,wizard-apply.log,patch_wiz.py}`

### 仍然没做（不抬高）

- Shift/拖动把引用扩成 `A1:B2`、点行号列标建 `A:A`/`1:1`、跨工作表点选建 `Sheet2!A1`；
- 向导里点区域填参数、分类筛选、帮助链接；
- 真实中文 IME 候选确认（本机 RDP 不可用，继续待验收）。

### 台账

- parents 61：已验收 24 / 实施中 3 / 待验收 27 / 待实施 7；
- slices 148：已验收 60 / 待验收 64 / 待实施 21 / 实施中 3（本轮未改变计数）。

## 2026-11-16 检查点三：UI-11.5 拖动/Shift 区域引用 + 行号列标引用

- HEAD `b00d923`（`feat(ui11): drag/shift range picking and whole row/column references`）。
- app 全量：**324 passed / 0 failed / 0 ignored**（9.71s）。

### 做完的

1. Shift+点：先点一格插入 `A1`，按住 Shift 再点另一格把刚才那串原地改写成 `A1:B3`（锚点在前，反向拖就是 `B3:A1`），
   引用框跟着扩，选区不动。
2. 按住拖动：按下即落锚点，指针移到别的格就 `replace_char_span` 原地改成「锚点:当前格」。
   自绘面板上 `Response::dragged()` 不成立，判定改用 `i.pointer.primary_down()` + `pointer_interact_pos()`。
3. 行号/列标：编辑公式时按列标插 `B:B`、按行号插 `3:3`，引用框覆盖整列/整行。
   这段必须排在「按下即提交草稿」之前，否则这一按先提交编辑；`clicked()` 帧 `press_origin` 已清空，要退回 `interact_pointer_pos()`。
4. `commit_edit` / `cancel_edit` 一并清 `pick_anchor` / `pick_span`。

### 实测

- 新增 4 条用例：`ui115_reference_text_helpers`、`ui115_shift_click_extends_reference_to_range`、
  `ui115_dragging_while_picking_rewrites_reference_to_range`、`ui115_header_click_inserts_whole_column_and_row_reference`。
- GUI：`SC_TYPE="6:=SUM(" + SC_REF_SIM="0,0,3,2,14"` → `buf="=SUM(A1:C4"`、`pick=(0,0,3,2)`、`spans=["5..10=#0"]`，
  截图里 A1:C4 套一圈蓝框、选区仍在 E1。`SC_REF_SIM` 现支持单格与区域两种写法。
- 应用内追加的指针事件赶不上当帧 `begin_pass`（`i.pointer.*` 读不到），所以删掉了刚加的 `SC_REF_DRAG`/`SC_HEAD_PICK` 两个无效探针；
  拖动的真实指针语义由 egui 集成用例保证（直接喂 RawInput 跑 `draw_grid_pane`）。

### 证据

- `implementation-evidence/ui11-range-ref.txt`
- `implementation-evidence/ui115-probe/{range.png,range.log}`

### 仍然没做

- 跨工作表点选建 `Sheet2!A1`（点另一张表页会先结束当前编辑）；
- 向导里点区域填参数、分类筛选、帮助链接；
- 真实中文 IME（RDP 不可用，待验收）。

## 2026-11-16 检查点四：QA-06.1 错误/空值/日期矩阵（Excel 与 WPS 实算对拍）

- 提交 `c0177c9`（`test(qa06): error/blank/date matrix against real Excel 16.0 and WPS 12.0`）。
- 夹具 `work/qa06/qa06.xlsx` 由 `scripts/qa06_fixture.py` 生成；公式不在 xlsx 里，由
  `scripts/qa06_excel_probe.ps1` 用 Excel COM 逐条写进 C 列再 `Calculate()`——
  openpyxl 直接写 `IFNA`/`DAYS` 会缺 `_xlfn.` 前缀，Excel 认成 `#NAME?`（第一版踩到）。
- 实测：`Excel 16.0：50 条；WPS=12.0`；`cargo test --test qa06_matrix` → **1 passed**，
  err 16 / blank 17 / date 17 全部与 Excel 一致。
- 比较口径：错误码与布尔按显示值严格比对；数字按 Excel `Value2` 相对 1e-9 比较
  （用 `Text` 会因格式把 11339.375 显示成 11339.38、日期显示成 `########`/`1/1/2025` 而产生假差异）。
- Excel 与 WPS 语义全一致，两处 `########` 只是 WPS 列宽不足。
- 证据：`implementation-evidence/qa06-1-error-blank-date.txt` 与 `qa06-1/{cases.json,excel.tsv,wps.tsv,probe-meta.json}`。
- 台账：QA-06 父项 → 实施中，QA-06.1 → 待验收；父母 24/4/27/6，切片 60/1/68/19（未抬高已验收数）。
- 下一步：QA-06.2 数组/动态数组与增量一致。

## 2026-11-16 检查点五：QA-06.3 财务/查找/文本矩阵 + 3 个真实缺陷

- 提交 `504c9e8`（`fix(calc): IPMT/PPMT sign, CUMIPMT pv guard, TEXT year-month-day tokens; extend qa06 matrix to 102`）。
- 矩阵从 50 条扩到 **102 条**（fin 16 / lookup 14 / text 22 新增），Excel 16.0 与 WPS 12.0 都实算。
- 对拍揪出并修掉 3 个真缺陷：
  1. `IPMT` 符号反（余额误用 `fv()`，它与 pv 反号）→ `-1250` 变 `+1250`；
  2. `PPMT` 被同一根因带错 `2860.46` → `360.46`；
  3. `CUMIPMT` 缺 `nper<=0 / pv<=0` 校验，Excel 返回 `#NUM!` 而本引擎照算 → 已补（CUMPRINC 同步）。
  另有 `TEXT(45351,"yyyy年m月d日")` 只输出 `2024年m月d日`：改成按记号扫描的 `format_date_tokens`。
- 容差写进用例：默认相对 1e-9，`IRR`/`XIRR`/`RATE` 用 1e-7（Excel 自己也是数值解）。
- 回归：`-p sheetcore --lib` 295 passed、`-p sheetcore` 101 个测试二进制 0 failed、`-p app --bin app` 325 passed。
- 证据：`implementation-evidence/qa06-2-fin-lookup-text.txt` 与 `qa06-2/{matrix.tsv,excel.tsv,wps.tsv,cases.json,probe-meta.json}`。
- 台账：QA-06.3 → 待验收；父母 24/4/27/6，切片 60/1/69/18（未抬高已验收数）。
- 下一步：QA-06.2 数组/动态数组与增量一致。

## 2026-11-16 检查点六：QA-06.2 动态数组溢出对拍 + 增量一致（QA-06 三切片齐）

- 提交 `f508125`（`test(qa06): dynamic-array spill blocks vs Excel and incremental/full recalc parity`）。
- 夹具 12 个数组公式锚点（E1/E10/…，间隔 ≥9 行）；Excel 实算经 `SpillingToRange` 导出块：
  SEQUENCE×2、SORT、SORTBY、FILTER、TRANSPOSE、`B1:B5*2`、UNIQUE 共 **8 块 40 格与本机 Excel 逐格一致**。
- `TAKE`/`DROP`/`HSTACK`/`VSTACK` 在本机 Excel 16.0 Build 20326 返回 **#NAME?**（该构建没有这些函数），
  矩阵里明确 SKIP：不算通过也不算失败，不拿“引擎实现了”冒充对拍通过。
- 增量一致：改 B1 后 `recalc_dirty` 只重算 **12 格**，130×8=**1040 格**与全量重算逐格相同。
- 坑：PowerShell `Set-Content -Encoding UTF8` 写 BOM → `&b.anchor[1..]` 报 "not a char boundary"；
  探针改 `UTF8Encoding($false)`，Rust 侧再 `trim_start_matches('\u{feff}')` 兜底。
- 证据：`implementation-evidence/qa06-2-array-spill.txt` 与 `qa06-2a/{blocks.tsv,anchor.tsv,probe-meta.json,qa06a.xlsx}`。
- 台账：QA-06.2 → 待验收，QA-06 父项三个切片齐 → 待验收；父母 24/3/28/6，切片 60/1/70/17（未抬高已验收数）。

## 2026-11-16 检查点七：C12.4 真 pivotTable 部件在 Excel/WPS 可用（单行+单列+单值）

- 提交 `9452130`（`fix(pivot): emit valid pivotCache/pivotTable parts Excel and WPS accept as editable`）。
- **先证伪再修**：旧实现生成的 xlsx 用 Excel 打开直接 `COMException: 不能取得类 Workbooks 的 Open 属性`——
  等于"写了 pivot 部件但 Office 打不开"。对照物是 Excel 自己造的 `excel-made.xlsx`（`scripts/c12_make_pivot.ps1`）。
- 四处硬伤：①`cacheField@name` 用了透视输出列标题而非源表表头；②`pivotCacheRecords` 空体却声明 `recordCount=3`；
  ③行/列 `pivotField` 缺 `<items count=n+1><item t="default"/></items>`；④`dataField@name` 变成 `求和项:求和项:金额`。
  另外 `rowItems/colItems` 的 `<x v=k/>` 指向不存在的 sharedItems 下标、`colItems` 缺总计列。
- 新增 `crates/core/src/pivot_parts.rs` 统一生成三份 XML（读源表建缓存、记录与索引自洽、自检不一致就 `Err`），
  `pivot_xml.rs` 改为调用它；`refreshedBy` 仍透传 `sheetcore:<session token>`，`pivot_save_lifecycle` 9 条全过。
- 实测：Excel 16.0 与 WPS 12.0 都报 `PivotTables=1`、`rows=[地区] cols=[产品] data=[求和项:金额]`，
  值 40/50/90、60/60/120、100/110/210 —— 是按 6 条 cache 记录重新聚合的，不是照抄静态值。
- 回归：`pivot_attachments` 6 passed、`pivot_save_lifecycle` 9 passed、`-p sheetcore` 全量 102 个二进制 0 failed。
- 台账：C12.4 待实施 → 实施中；parents 24/3/28/6，slices 60/70/2/16（已验收未抬高）。
- 剩余：多行/多列/多值字段仍未写进 pivotTable 部件（嵌套 rowItems、多个 dataFields）。

## 2026-11-16 检查点八：C12.4 多字段真透视 + 接进保存链路

- 提交 `41b448b`（多字段生成器）、`66d17f1`（保存队列接线）。
- 先让 Excel 造三份黄金样本（`scripts/c12_make_pivots_multi.ps1`）：A=2 行字段、B=1 行+1 列+2 值、C=2 行+1 列。
  关键结构：`colFields` 末尾 `<field x="-2"/>`（数据伪字段）、每个列路径 K 个 `<i>`（第 2 个起 `i="1"`）、
  组尾 `<i t="default"><x/></i>`、`firstDataCol=行字段数`。
- 新增 `crates/core/src/pivot_parts2.rs` 生成多字段三件套；`pivot_xml.rs` 抽出 `write_parts` 并加 `attach2_for_session`。
- Excel 16.0 / WPS 12.0 双双确认：A `rows=[地区,产品]`、B `cols=[产品,值] data=[求和项:金额,求和项:数量]`、C `cols=[数量]`，
  值与手算一致（华东A=40、华北B=60、B 用例 90/12 与 120/15、总计 210）。
- 接线：`Workbook::pivots_pending2` + `xlsx::save` 附挂 + `sheet_ops` 下标重映射 + 面板 `apply_pivot2` 入队（同位置刷新不重复排队）。
- 测试：`pivot_multi_parts` 4 passed、app `multi_field_pivot_from_panel_is_queued_for_real_parts` 1 passed；
  全量 `-p sheetcore` 103 个二进制 0 failed、`-p app` 326 passed。
- 剩余：多列字段（≥2）与折叠布局 `rowItems`、非求和聚合的缓存属性、Excel 改字段后存回的保真。

### 检查点八补充：两个列字段 + 保真
- `5b4719c`：列轴推广到 N 层（`<i r="k">` 重复外层、组小计 `<i t="default">`、grand）；
  踩坑：`pivotFields` 只给第一个列字段标 `axisCol` → Excel 报「不能取得类 Workbooks 的 Open 属性」，
  必须把所有列字段都标轴。修后 Excel/WPS 均 `cols=[产品,数量]`，值 10/30/60 与汇总 40/60/100/110/210 正确。
- `61132d7`：保真链路 —— Excel 打开我们的多字段透视并另存 → 我们 `xlsx::load`+`save` → Excel/WPS 仍 `PivotTables=1`。
- 回归：`pivot_multi_parts` 5 passed；`-p sheetcore` 103 二进制 0 failed；`-p app` 326 passed。

### 检查点九：C12.4 收口（可编辑往返 + 非求和聚合 + 多列多值）
- `3432c15`：`dataField@subtotal`（count/average/max/min）+ 无列字段的纯 `-2` 值轴。
  发现的真实缺陷：界面能选计数/平均/最大/最小，但部件缺 `subtotal`，Excel 一律按求和重算。
- `bb19c63`：多列字段 × 多值字段（`colFields` 末尾 `-2`、每叶按值展开、小计/总计按值展开）+ 可编辑往返证明。
- 可编辑往返链路：Excel 打开我们的透视 → `AddDataField(最大值金额)` → `SaveAs` → 我们 `load+save` →
  Excel 与 WPS 都仍是 3 个数据字段（`求和项:金额,计数项:金额,最大值金额`）。这才叫「可编辑往返」，不是「能打开」。
- 用例：A(2 行) B(2 值) C(2 行 1 列) D(2 列) E(5 聚合纯值轴) F(1 行 1 列 2 值) G(2 列 2 值)，全部 Excel+WPS 双端实测。
- 回归：`pivot_multi_parts` 7 passed；`-p sheetcore` 103 二进制 0 failed；`-p app` 326 passed。
- 台账：C12.4 → 待验收，C12 → 待验收（C12.1 已验收、C12.2/12.3 待验收）。
- 未做且已记录：折叠布局（界面无开关）、计算字段/显示值方式、WPS 内改字段往返。

### 检查点十：UI-11.6 跨工作表取引用
- `42aa56f`：`edit_sheet`/`edit_sel` 记住编辑所属表与目标格；公式编辑中点别的表页只切视图
  （`switch_sheet_for_ref`），提交/取消前 `return_to_edit_origin()` 切回原表原格；
  引用标签统一 `qualify_ref()` 加 `表名!` 前缀，名字含空格/标点/数字开头按 Excel 规则加单引号。
- 探针（真 GUI）：`[ui116] pass6 sheet=1 label=Sheet2!B3 buf="=1+Sheet2!B3"` → `pass12 formula==1+Sheet2!B3 value=43`；
  第 8 帧截图里名称框 A1、公式栏 `=1+Sheet2!B3`、底部 `Sheet2` 激活、状态栏「单元格模式 输入 / 已插入」。
- 测试：4 条 `ui116_*`；app 全量 **330 passed**。
- 边界：跨表拖动扩区域未做（需远端锚点），已写进 `not_done`。

### 检查点十一：QA-01.3 特性矩阵（样式/CF/DV/表格/命名/隐藏/冻结）
- `56f9cc2`：`crates/core/examples/qa01_features.rs` 造夹具，`scripts/qa01_features_probe.ps1` 双端逐项读对象模型。
- Excel 16.0 与 WPS 12.0 的 16 项读数完全一致：加粗/底色、`#,##0.00`+下边框、
  CF 2 条（cellIs / colorScale）、DV 列表+整数、表格 T1 `$A$1:$B$5`、`MyRange`、
  冻结 1/1、隐藏第 7 行与 E 列。
- Excel 另存 → 我们 `load+save` → 双端复测全部保留（往返成立）。
- 抓出并修掉三个真实缺陷：dxfs 落在 `<extLst>` 之后（Excel 判包损坏）、
  表格 `displayName` 用了单元格引用 T1（Excel 拒开）、新建工作簿冻结窗格保存即丢
  （没有 `<sheetViews>` 就 return）。5 条回归测试入 core。
- 探针坑：WPS 里 `ActiveWindow.SplitRow = SplitRow` 会清掉列分割，读到假 0，已删该行。
- 测试：core 103 个测试二进制 0 failed；app 330 passed。
- 剩余：图片/图形、外部链接、.xls/.xlsm 三类夹具；QA-01.3 与 QA-01 父项仍「待验收」。

### 检查点十二：QA-01.3 收口（图片/形状/外链、.xlsm、.xls）
- `3901d39`：加载不再把 textboxes 标脏 —— 之前保存会用文本框模型重写 drawing，
  矩形变成 `txBox="1"`、off/ext 归零、style 丢失；现在 drawing1.xml 与图片逐字节保真。
- `3901d39`：`.xlsm` 基准另存 `.xlsx` 后 Excel 报「文件格式或文件扩展名无效」——
  `[Content_Types].xml` 仍声明 macroEnabled；新增 `fixups::normalise_workbook_ct` 按输出
  扩展名归一（带 VBA 写 .xlsx 直接报错，不静默丢宏）；临时名 `.~x.xlsx.saving-<pid>`
  的扩展名要还原，否则 .xlsm 被当未知格式。
- `3901d39`：`.xls` 打开后另存 `.xlsx` 崩「读取基准工作簿目录失败」——二进制基准不是 zip，
  现在按全新文件重建。
- 实测：media.xlsx（1 图片 + 1 矩形 + 外链 42/EXT）经我们保存后 Excel/WPS 均
  pictures=1 shapes=2 links=1，shape2 仍是 msoAutoShape 80×40。
- `features.xlsm` → 我们另存 `.xlsx` → Excel 16 项读数与原始 `.xlsx` 完全一致。
- `.xls` 边界（写进 not_done）：值/数字格式/粗体保留，CF/DV/表格/名称/冻结/隐藏 与
  填充/边框颜色不保留 —— calamine 只给值。
- 测试：core **105** 个测试二进制 0 failed（`qa01-core3.log`）；app **330 passed**（`qa01-app2.log`）。

### 检查点十三：QA-01.3 图形矩阵（组合/连接线/内嵌图表 + 改字保存）
- `bcef26b`：解析与重写都改成「组合/连接线不归我们管」。修前改一个字会出现
  shapes 4→6（多出 type17 的 LinkAB/GroupAC）、矩形变文本框（`txBox="1"`、off/ext 归零、
  `xdr:style` 消失）。
- 命中形状改成**原地改**：只换 `<a:t>` 与 `sz/b/i`/`srgbClr`，Excel 的 spPr/prstGeom/style 全留；
  空形状插字要放在 `<a:endParaRPr>` 之前（否则 Excel 忽略）。
- 独立 `<xdr:sp>` 以模型为准（清空文本框时形状必须消失），外来块无条件保留 ——
  既有 `deleting_all_textboxes_removes_the_shape` 继续守住。
- 实测：shapes.xlsx（矩形 + 连接线 + 2 项组合 + 内嵌柱状图/标题「销量」）只读往返时
  drawing/chart 部件逐字节一致；改 BoxB 文字后 Excel/WPS 均 shapes=4、BoxB 仍 type1、
  组合 2 项、图表 1 系列标题在。
- 顺手补 `.gitignore`：`tests/fixtures/qa01/formats/*` 之前被 `*.xlsx` 规则挡住没入库，
  别人 clone 后测试会缺夹具。
- 测试：core **106** 二进制 0 failed（`qa01-core5.log`）；app **330 passed**（`qa01-app3.log`）。
- 剩余：SmartArt / 三维形状 / 图表形状未覆盖；`.xls` 结构性对象口径待确认。

### 检查点十四：QA-01.3 SmartArt / 三维形状
- `1f4517e`：SmartArt 夹具（5 节点）+ 三维矩形（Depth 30 / rotX 25 / rotY 15）。
- 只读往返：`xl/diagrams/{data1,layout1,quickStyle1,colors1,drawing1}.xml` 与 `drawing1.xml`
  逐字节一致；Excel 读数与源相同（nodes:5、texts:节点一|节点二、depth:30）。
- 改 Box3D 文字后保存：文字进、`<a:scene3d>`/`<a:sp3d` 在、SmartArt 图形框在。
- WPS 对照：源文件在 WPS 里就是 `nodes:1 default`（且把普通形状也报 HasSmartArt），
  产物读数与源一致 —— 属 WPS 读取端限制，登记进 not_done 而不是当成我们的缺陷。
- 测试：core **106** 二进制 0 failed（`qa01-core6.log`）；app **330 passed**（`qa01-app4.log`）。

### 检查点十五：QA-01.2 manifest 驱动三方 runner
- `3c6767f`：`scripts/qa01_matrix_runner.py` + `scripts/qa01_manifest.json`，
  对每个夹具跑「源快照 → 我们 load+save → 副本快照」，Excel 与 WPS 各一遍，逐字段 diff。
- 快照字段：工作表/UsedRange、CF、DV、表格、名称（名字→引用）、页面设置、冻结、隐藏行列、
  图片、形状（类型/名字/宽高/组合项/SmartArt 节点）、图表（系列/标题）、外链（按文件名）、透视（字段/方向）。
- 12 个夹具 0 处意外差异；`.xls` 6 处为已登记损失（两侧一致），`.xlsm` 0 处。
- runner 自身修了两个比对口径（非产品缺陷）：外链按完整路径比会因副本目录不同而误报；
  名称按位置比会因我们追加 `_sc_manual_hidden` 记账名而错位 —— 改按字典比并过滤 `_sc_`。
- 证据：`qa01-matrix-runner.txt` + `qa01-matrix/{matrix-full.json,matrix-summary.tsv}`。

### 检查点十六：UI-30.1 命令可达性（宏入口 / 未命中留痕 / 隔离副本）
- `880e046`：审阅页签新增「宏」组（MacroView / MacroRun / MacroExport），
  三条 dispatch 臂在无宏文件时给明确提示；兜底分支改成写 `unhandled_action` +
  状态栏「这个命令还没有接入：X」（不再只靠 `SC_RIBBON_LOG`）。
- `run_macro_via_excel` 改为先复制到 `%TEMP%\sc_macro_run_<pid>_<ms>.<ext>` 再跑，
  成功后状态栏给副本路径并注明原文件未改动；不再写回用户文件。
- 测试从「源码子串包含」升级为「从 match 臂精确抽命令名 + 命令注册表 + 前缀家族」：
  `every_action_is_dispatched` 缺失即失败；新增 4 条（未知命令留痕、宏三按钮提示、
  有宏时打开运行框、审阅页签有宏组）。app **333 passed**、core **106** 二进制 0 failed。
- 真机：`SC_TAB=6` + `SC_RIBBON_ACTION` 跑出三条状态；截图确认「宏」组可见。
- 未做：全部启用按钮的穷举点击矩阵；旧菜单四个函数（已无调用点）尚未删除，
  删除前要核对 `set_freeze/export_pdf/open_compare` 与 Ribbon 侧等价。

### 检查点十七：UI-30.1 旧菜单清理（先接能力再删代码）
- `36d2966`：删除前逐项核对，发现三处能力只在旧菜单里：
  并排比较（open_compare）、条件格式快捷规则（突出显示/项目选取/数据条/图标集/色阶/清除）、
  数据验证工具（圈释无效/清除）。先补 Ribbon 入口再删：
  视图→窗口组 CompareBooks / CompareSync；条件格式菜单 CfQuick（窗口直接列快捷规则）；
  数据页签 DvQuick（窗口渲染验证工具）。
- toolbar_cf/toolbar_dv 拆成 `*_items`，功能区菜单与窗口共用同一份实现，不复制逻辑。
- 删除 toolbar_notes/view/data/page + set_freeze + export_pdf 共 **326 行**；
  大括号配平脚本删除（识别注释/字符串/字符字面量），不是按行号切。
- 测试：app **336 passed**、core **106** 二进制 0 failed；真机截图确认快捷规则窗口内容。

### 检查点十八：UI-30.1 功能区点击矩阵
- `b6779b8`：`click_matrix_every_ribbon_action_is_handled` 从 XML 收集 283 个动作，
  每个动作新建干净 App 真跑 `ribbon_action`，落兜底即失败（`unhandled_action`）。
- 269 个动作跑完 0 漏网；14 个跳过并逐条注明原因（rfd 原生对话框、共享监听、联网检查）。
  跳过名单必须是 XML 里真实存在的动作，拼错即失败（初版凭印象写的 3 个假名字被当场抓出）。
- 过程中发现 `CheckUpdate` 会联网卡死、`ImportWorkbook`/`Hyperlink`/`OpenInNewTab` 会弹对话框。
- 测试：app **337 passed**、core **106** 二进制 0 failed。
- 边界：矩阵只证明「有实现、不落兜底」，不证明业务效果；14 个跳过项需真人点击。

### 检查点十九：QA-07 首次性能基线（并修掉稀疏远端内存缺陷）
- `acb5f87`：新增 `crates/core/examples/perf_probe.rs`，现场生成 values/chain/sparse × 1万/10万/100万
  共 9 组样本，记录 build/load/recalc/改一格/save/另存/RSS/槽位数。
- **真实缺陷**：`Store::Page::ensure` 把页内所有列都按整页补槽，远端稀疏行 1 万个散点 →
  3,423,112 个槽位（约 137MB），RSS 169MB。改成只补目标列后 slots 855,778、RSS 57MB、build 48→13ms。
  两条回归：`sparse_far_rows_do_not_allocate_full_page_slabs`、`dense_and_sparse_columns_coexist_in_one_page`。
- GUI 层（release app）加 `[boot] first_frame` / `[open] editable` 打点，`SC_LOAD_PROF=1` 开启；
  与 `SC_SHOT/SC_SHOT_EXIT` 配合可无人值守跑：1M 值首帧 p50 0.64s、打开→可编辑 p50 4.09s、峰值 190.5MB。
- 阈值已冻结进 `qa-requirements.md` QA-07 验收段（含退化上限），证据 `qa07-first-baseline.txt`。
- 测试：core **106** 二进制 0 failed、app **337 passed**。
- 未做（不记通过）：滚动→呈现 p50/p95、冷启动（清系统缓存）、保存期间可操作性。

### 检查点二十：QA-07.2 滚动帧时延
- `d23d407`：`SC_SCROLL_PROBE=1` 装载完成后连续 120 帧每帧下移 60px 并 `request_repaint()`，
  同时记「注入→下一帧开始（含呈现/vsync）」与「本帧 update() 墙钟」。
- 结果：values 100k 16.65/16.97ms；values 1M 20.13/30.25ms；chain 1M 29.33/30.25ms；sparse 1M 16.66/16.77ms。
- **踩坑**：第一版没 `request_repaint()` → 每帧 ~500ms，那是 eframe 等事件的空闲等待，
  不是渲染耗时。这条已写进证据，避免把「等事件」误当「滚动卡」。
- 阈值追加进 `qa-requirements.md`（滚动帧间隔 p95 30.25ms，上限 36ms）。
- 口径边界：程序改滚动位置，非 OS 滚轮事件；不含 GPU 呈现完成时刻。QA-07.2 置「待验收」。

### 检查点二十一：QA-07.3 取消、进度、保存期可操作性
- `e7f0218`：core 加 `recalc_all_force_impl(wb, cancel, progress)`（每 256 个公式查一次取消，
  取消时不写回并置 `calc_pending`）；app 后台重算带 done/total，状态栏「正在重算 d/t（Esc 取消）」，
  Esc 与探针共用同一 cancel 分支，收尾消息 `RecalcCancelled`；保存中状态栏显示已用时间。
- `Store::Page::cols` 改 `Arc<Vec<Cell>>` 写时复制：整体 clone 只加引用计数，改到哪列才复制那列。
- 实测 chain 1M：重算 3.15s（96 帧 p50 29.98ms）、300ms 取消 0.39s 收尾且不写回、
  保存 11.51s（120 帧 p50 29.88ms）——窗口全程可操作。
- **未解决**：保存瞬时峰值 core 1,186MB / GUI 1,923MB，根因是 umya 整份副本 + 序列化缓冲；
  COW 只省掉 clone 那份（1993→1923MB）。
- 测试：core 106 二进制 0 failed、app **339 passed**。

### 检查点二十二：UI-24.2 可访问树接入（accesskit）
- `284f63c`：eframe 开 `accesskit` feature；`ui_kit::a11y_name` 给自绘控件补
  WidgetInfo + accesskit 节点（Button/label/disabled/toggled），已接功能区按钮与 pill；
  `Dlg::show` 注册 Role=Dialog + 标题；网格 `publish_grid_a11y` 发布活动单元格
  （Role=Cell，名字「A1，0，已选中」，带值与公式）。
- 外部验证：PowerShell UIAutomationClient 枚举窗口子树，功能区按钮从「无名字」变成
  「剪切（Ctrl+X）/加粗（Ctrl+B）/…」，对话框显示 `Pane 数据验证` 及其按钮，
  网格是 `DataItem A1，0，已选中`。原始 dump 在 `implementation-evidence/ui24/uia-dlg3.txt`。
- 边界：只覆盖按钮/pill/对话框标题/活动单元格；下拉与菜单项、状态栏未补；
  UI-24.1 焦点锁与恢复未做；未用 Narrator 实读（RDP 下无法自动化）。
- app 339 passed。

### 检查点二十三：UI-24.1 模态焦点 / 关闭恢复 / 默认按钮
- `2218a50`：`Dlg::show` 打开首帧记住原焦点、关闭时还回去；帧末 `set_modal_layer(对话框层)`，
  关闭帧把模态层降回 background（否则下一帧背景控件会 `surrender_focus` 冲掉还回去的焦点）；
  新增 `drag_only_sense()` 给标题栏/缩放手柄/遮挡层去掉 FOCUSABLE。
- 真实失败→通过：先报「Tab 之后焦点没有进对话框」，根因是遮挡层与标题栏都可聚焦；
  再报「对话框内容键盘不可达」，最后报「关闭后焦点没有回到触发控件」，逐条修完。
- 测试：`modal_dialog_traps_tab_focus_and_restores_it_on_escape`、
  `default_button_answers_enter_only_when_no_field_has_focus`；app **341 passed**。
- 边界：未用物理键盘在 GUI 复核（RDP），Shift+Tab 未单独断言。

### 检查点二十四：UI-24.3 网格坐标与选区语义
- `d8a8619`：网格发布 `Role=Grid` 汇总节点（工作表/选区范围/几行几列/活动单元格）；
  可见列发 `ColumnHeader`（`列 B，已选中`）、可见行发 `RowHeader`（`第 2 行，已选中`），
  带 accesskit bounds（已乘 pixels_per_point）。
- 外部 UIA 实测：COUNT 115→168；`DataGrid 工作表 Sheet1，选区 B2:C4，3 行 2 列，活动单元格 B2`；
  列 B/C 标已选中、列 D 不标；第 2/4 行标已选中、第 5 行不标；bounds 列宽 67、行高 18 与界面一致。
- 已知限制：accesskit 0.17 不提供 SelectionItemPattern（UIA 报 no-pattern），选中态写进名字；
  选区变化不发通知事件；无冻结/合并特殊语义。
- app 341 passed。

### 检查点二十五：UI-24.2 控件语义补齐
- `1e72505`：新增 `ui_kit::a11y_role`；工作表标签发 `Role=Tab`；自绘下拉/画廊条目/画廊箭头/
  拆分箭头/名称框/标题栏搜索框/字体字号输入框全部补名字；`CellStylesGallery` 补 `label`
  （否则箭头只能叫 id）；网格节点补 bounds（原来 UIA 里是 ∞）。
- 关键实测：`Edit 字体=思源黑体`、`Edit 字号=11`、`Edit 名称框=A1`、`TabItem Sheet1`、
  `DataGrid … at=271,463,1423,574`、`Button 常规/标题/强调/好/差`。
- 边界：右键菜单/筛选下拉/颜色面板未逐个验；禁用态未验出 IsEnabled=false；公式栏仅编辑态有 Edit。
- app 341 passed。

### 检查点二十六：右键菜单语义
- 右键菜单每行发 `Role=MenuItem`（名字带快捷键与「有子菜单」），粘贴图标条 5 格各发 Button；
  `SC_CTX_MENU=600,600` 实测：`MenuItem 剪切，Ctrl+X at=949,826,246,26`、`Button 粘贴公式` 等。
- **查出功能差异**：功能区与右键菜单没有运行期禁用态，`Ctl.enabled` 恒 true，
  剪贴板为空时「剪切/复制/粘贴」照样可点；因此 a11y disabled 路径无法用真实按钮验证。
- app 341 passed。

### 检查点二十七：粘贴按钮随剪贴板灰化
- `has_pasteable()`（查格式不读数据、500ms 缓存）接入 `ctl_state` 的 Paste 系列；
  空剪贴板 `Button 粘贴 enabled=False`，Set-Clipboard 后 `enabled=True`（外部 UIA 实测两向）。
- 顺带补齐大号拆分按钮主体的可访问名（此前只有箭头有名字）。
- app 341 passed。

### 检查点二十八：QA-05.2 四档 DPI
- `a3cd75f`：新增 `SC_PPP`；`run_dpi_probe.ps1` 在逻辑 1280×800 下跑 1.0/1.25/1.5/2.0，
  物理分辨率 1280×800→2560×1600；粘贴按钮物理尺寸 42×76→84×152 精确按 ppp 缩放。
- **真崩溃**：960×600@1.5 panic `min > max, min = 24.0, max = 17.0`，根因滚动条
  `clamp(24.0, span)`，span<24 时违反 clamp 前提；已修并加 zoom_bar 退化矩形保护。
- 边界：SC_PPP 是 egui 的缩放机制，未做真实系统缩放/多显示器；vision 服务超时，
  没有人工视觉基线，只有像素差与尺寸核对。
- app 341 passed。

### 检查点二十九：QA-05.1 三档尺寸
- `run_size_probe.ps1` 跑 1440×900 / 1024×768 / 760×420，三张 PNG 尺寸精确，零尺寸节点 0。
- **真缺陷**：760 宽时「更多 ▾」画到窗口外 11px（771>760）。折叠条件没给「更多」预留宽度；
  改为 `used + w + 60 > avail`，并加 `scope_builder` 钉右侧兜底。复测 734<760。
- UIA `InvokePattern` 真点「更多 ▾」，弹出 单元格/编辑/加载项，证明折叠不是摆设。
- 未解决：状态栏缩放控件可访问矩形底部溢出 2-5px（未确认视觉裁切）；公式栏非编辑态无节点。
- app 341 passed。

### 检查点三十：QA-05.3 区域差异门槛与五态
- `scripts/qa05_region_diff.py` 按六区域比对；门槛写入 `qa05-visual-spec.md`；
  1.0 vs 1.25/1.5 六区域全 PASS，两个合成反例只在对应区域 FAIL。
- 新增 `five_interaction_states_are_visually_distinct`：直接读绘制形状的填充/文字/描边色，
  覆盖静止/hover/pressed/selected/disabled/focus，五色两两距离 ≥6。
- **真缺陷**：功能区大按钮/小按钮没有键盘焦点指示（只有对话框有）；新增
  `ui_kit::focus_ring`，`big_sized`/`color_button`/字体样式切换三处接上。
- `SC_DUMP` 增加 `selrect`，实测缩放 1.0/1.25/1.5/2.0 选区 134/167.5/201/268 宽，无 1px 漂移。
- app **342 passed**。

### 检查点三十一：QA-04.1 原生快捷键（部分达成）
- **真缺陷**：Ctrl+S/Ctrl+Z/Ctrl+Y/Ctrl+Shift+Z 从没绑键（只有 UI 能点）。新增
  `handle_file_shortcuts` + 回归 `ctrl_z_and_ctrl_y_drive_undo_and_redo`。
- 原生键盘实测（PostMessage WM_KEYDOWN → winit）：F2 出 `Edit 编辑栏`、Esc 节点消失、
  Enter 选区 A1→A2、Down/Down/Right → B4。
- **阻塞**：本机无交互桌面（GetForegroundWindow=0），SendInput/WM_CHAR/WM_IME_CHAR/
  修饰键/鼠标消息全部无效 → QA-04.1 只能跑无修饰键子集，保持「实施中」。
- app **343 passed**。

### 检查点三十二：QA-04.3 多文档/后台任务（已验收）
- **真缺陷**：后台重算结果只按 `wb_epoch` 认领，而 epoch 是每个文档各自计数 ——
  A 文档的 epoch 可能恰好等于 B 文档切回来后的 epoch，A 的重算结果会覆盖 B。
- 修法：`RecalcedWb` 带 `doc_gen`，接收侧 `epoch && doc_gen` 双条件；切标签/开新标签前
  置位 `tasks.recalc_cancel`，不等它回来。
- 5 条回归：值/选区/滚动/脏、撤销历史归属、后台保存归属、切标签取消重算、
  epoch 撞车时按 doc_gen 丢弃（含「换回正确 doc_gen 必须被接受」反例）。
- 边界：切标签走 API 不是真人点标签（单文档时无文档 TabItem，开第二个要走 rfd）。
- app **348 passed**。

### 检查点三十三：UI-29.1 design token + 跨 DPI 修正
- 新增 `tokens.rs`（space/radius/stroke/text/size/icon），功能区组、分组启动器、
  主/次按钮、pill、工作表标签接入；分组名间隙等按 Excel 实测值定档。
- **真缺陷**：`a11y_name_rect`、网格 `Role=Grid`、行列头三处把逻辑点又乘了一次
  `pixels_per_point`，accesskit 树本身已经乘过 → 150% 下读屏拿到 94×144 的粘贴按钮
  （应为 63×96）、2841 宽的网格（比窗口还宽）。UI-24.2 只在 100% 验过所以没暴露。
- 四档实测：125%/150% 各 101 个元素全按倍率；200% 55 个达标（41 个网格类因屏幕
  高度裁切排除）；`compare_scaling.py` PASS。
- app **349 passed**；证据 `ui29-tokens.md` + `ui29-100.png` / `ui29-150.png`。

### 检查点三十四：UI-29.2 图标四档自检（已验收）
- 138 个图标的形状级自检抓到**两个真缺陷**：`Comma` 用 21 单位字号画逗号，尾巴下伸到
  y≈26（16px 档压到标签）；`FormulaView` 用字号画 "fx"，行高把顶边顶出盒子 2.5px。
  两处都改成矢量笔画。
- 像素级对照表：16/20/24/32 + 24@150% 各 138 格，空白 0、糊满 0；墨量中位 0.254→0.203。
- 线宽下限 1px 自检通过；app **352 passed**；证据 `ui29-icons.md` + 5 张对照表 PNG。

### 检查点三十五：UI-29.3 状态视觉 + QA-07.2 时延复核
- UI-29.3：新增 `state_view` / `state_banner` / `loading_overlay`（token 化），接入
  查找全部 0 处、加载问题对话框、打开/重算遮罩；4 条组件+集成测试；截图 `state-empty.png` / `state-loading.png`。
- QA-07.2 用冻结阈值复核：core 打开 1.469s/4.489s、重算 1.495s、保存 2.07s/7.01s；
  GUI 首帧 0.727s、可编辑 4.090s、峰值 192.4MB；chain 峰值 731.1MB —— 全部在限内。
  chain 打开→可编辑 6.42s（基线 6.09s，该场景无上限）如实记录。保存峰值仍归 QA-07.3。
- app **356 passed**。切片 65 已验收 / 5 实施中 / 77 待验收 / 1 待实施。

### 检查点三十六：QA-04.2 剪贴板三方互通
- 新增 `office_clipboard::write_office_clipboard`（Win32 一次写 XMLSS/HTML/UnicodeText）、
  `build_xmlss`（公式 → `ss:Formula` 属性、日期 → `DateTime`、文本保前导零、合并轴正确）。
- 四个方向逐格比对：Excel→app 15/15、WPS→app 15/15、app→Excel 15/15（含公式）、
  app→WPS 与 Excel→WPS 逐格相同（WPS 自身丢公式，已用对照组证明）。
- 修 4 个真实缺陷：只写 HTML 无 XMLSS（公式丢）、MergeAcross/MergeDown 轴写反（Excel 整块拒粘）、
  按「像数字」强转（00123→123）、CF_HTML 缺完整文档（Excel 粘不动）。
- app 357 passed。QA-04.2 仍为实施中：IME 端到端受无交互桌面阻塞。

### 检查点三十七：UI-24.1 三项收口 + QA-05.1 验收
- UI-24.1：Shift+Tab 反向循环用例（egui 焦点迁移在下一帧，断言要跨帧）；3 处裸主按钮
  （移动复制工作表、函数向导、排序/分列「执行」）改 `primary_button`，加源码级扫描用例；
  对话框内 Tab 到视口外控件自动 `scroll_to_rect`（反向验证：去掉滚动行用例失败）。
- QA-05.1 已验收：状态栏 100% 按钮 24px→18px（UIA 底边 1240>1238 → 951<952）；公式栏空闲态
  补 `Role::TextInput` + value（UIA `Edit 编辑栏 value=00123`）；760 档「搜索框」前提不成立
  （功能区无搜索框，命令搜索走 Ctrl+Q）；建 1440×900 golden 基线，两张逐字节相同，六区域 0.00。
- app 360 passed。slices 67 已验收 / 5 实施中 / 76 待验收。

### 检查点三十八：QA-05.3 验收 + UI-25 密集选区
- QA-05.3 已验收：2.0 档用 1280×720 逻辑 + 2560×1440 物理窗口，缩回后六区全 PASS
  （mean 1.33–8.63）；记录 `SC_PPP` 只改像素密度不改窗口的坑（同窗口比 2.0 逻辑视口会全 FAIL）。
- UI-25：1M 行选区状态栏统计 p50 0.91ms / p95 1.72ms，结果与同步路径一致；修真实缺陷
  「异步装载前建的统计任务永不重建」→ key 加 wb_epoch；新增 `large_selection_stats_are_not_empty`。
- 640×360 无越界（632/640）；折叠菜单弹出层不进 accesskit 树，转 UI-24.2。
- app 361 passed。slices 68 已验收 / 5 实施中 / 75 待验收。
