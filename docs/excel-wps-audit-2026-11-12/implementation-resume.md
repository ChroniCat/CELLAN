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
