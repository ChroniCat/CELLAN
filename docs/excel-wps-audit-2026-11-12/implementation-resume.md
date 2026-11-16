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
