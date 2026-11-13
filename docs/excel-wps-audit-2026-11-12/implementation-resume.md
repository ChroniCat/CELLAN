# 恢复记录

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
