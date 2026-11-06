# 原生重写：路线与里程碑

目标：不基于 Chromium 的桌面表格客户端，直接读写内网共享盘（SMB/UNC）上的 `.xlsx`，同一文件多人同时编辑、实时同步。

## 技术栈（已定）

| 层 | 选型 | 理由 |
|---|---|---|
| 语言 | Rust | 内存占用低、无 GC、单二进制分发 |
| 界面 | `egui` + `eframe`（wgpu 后端） | 原生绘制，无 Chromium；实测内存见下方"实测数字" |
| 表格渲染 | 自绘虚拟化网格（只画可见区） | 100 万单元格不卡 |
| 单元格模型 | 稀疏存储 `HashMap<CellRef, Cell>` | 空表不占内存 |
| xlsx 读写 | `calamine`（读）+ `umya-spreadsheet`（写，保留样式） | 先求能打开/保存不损坏 |
| 公式 | 自研解析器 + 求值器，先 30 个函数 | 可控、可对拍 |
| 协作 | `yrs`（Yjs 的 Rust 实现）+ 自建传输 | CRDT，断线合并 |
| 传输 | TCP/WebSocket，局域网直连 | 一个进程既当客户端也能当宿主 |

## 里程碑

### v0.1 能打开、能改、能存（本次）
- 启动窗口，虚拟化网格
- 打开 xlsx（UNC 路径直接读）
- 单元格编辑、方向键导航、Enter 提交
- 保存回原文件
- 验收：用 Excel 打开保存后的文件，内容正确

### v0.2 本地文件树
- 左侧目录树，直接浏览 `\\192.0.2.41\shared`
- 双击打开、最近文件

### v0.3 公式引擎 ✅ 2026-11-06
- 词法/语法：完整运算符优先级（比较 < & < +- < */ < ^ < 一元 < % < :）、区域、跨表引用、绝对引用、错误字面量
- 求值：短路 IF / IFERROR / IFS；循环引用检测；打开与每次编辑后自动重算
- 函数：**174 → 345 个**（第二轮加 171：统计分布 40+、工程 30+、高级财务 16、数据库 12、杂项 30+）（数学三角 55、统计 40、文本 26、逻辑 11、查找 12、日期 21、财务 12、信息 13，含别名）
- 覆盖测试：21 个用例，含 470/90/560/632.8 这类真实算式、日期序列（1900 闰年 bug）、跨表、循环引用
- 已修 bug：calamine 的 `cells()` 返回的是相对 Range 左上角的坐标，之前没加偏移，导致所有公式被挪格；已加回归测试
- 已修 bug：删除单元格后保存不会从文件里消失；改为记录 `cleared` 集合并真正删除
- **动态数组（第三轮）**：值类型扩展 `FValue::Array`；溢出写入相邻单元格（`#SPILL!` 占用检测、重算前清理）；落盘只存锚点公式，溢出值不写入文件
  - 函数：SEQUENCE / RANDARRAY / TRANSPOSE / UNIQUE / SORT / SORTBY / FILTER / TAKE / DROP / HSTACK / VSTACK / CHOOSECOLS / CHOOSEROWS / TOROW / TOCOL / EXPAND / MMULT
  - 比较运算支持区域广播（`B1:B4>10` 得到 4 个布尔值）
  - `LET` 用局部环境实现（标识符解析 → Node::Name → env_stack）
- 尚未支持：LAMBDA/MAP/REDUCE/BYROW（需要函数值）、T.TEST/F.TEST/CHISQ.TEST、LINEST/TREND、OFFSET/INDIRECT、CONVERT（单位换算）
- 第二轮修的两个 bug：① `numbers(args,0)` 会把 k/p 这类标量参数当数据（LARGE/TRIMMEAN/PERCENTILE 全错），新增 `first_numbers`；② erf 近似在 0 附近有 1.5e-8 误差，改为 |x|<2 用级数展开

### v0.3 公式
- 30 个常用函数 + 单元格引用 + 区间
- 与 Excel 对拍（同一批公式，结果逐格比较）






















### v1.16 图标集 ✅ 2026-11-06

- `CfKind::IconSet { set, reverse, thresholds, show_value }`；`IconMark` 把
  `3TrafficLights1` 这类名字拆成（形状, 级数），共 8 类形状：箭头/红绿灯/等级/旗帜/符号/
  四分之一圆/红黑/方块。
- 读的时候**属性在子元素上**：`<cfRule type="iconSet">` 里面才是
  `<iconSet iconSet="3TrafficLights1" reverse="1" showValue="0">`，一开始照着 cfRule 的属性读，
  结果所有列都被当成默认的 3Arrows。
- 求值：阈值是各档下限，逐档比较；`reverse` 把档位翻转。
- 绘制：图标画在单元格左侧（8–14px 自适应行高），文字自动右移 18px 不重叠。
- 工具条「条件格式 → 图标集」可新建 8 种常用集，阈值按等分百分比。

**顺带修掉一个真 bug**：换文件时 `doc_gen` 归零，而 `cf_stats_gen` 也是 0，
于是「统计已经算过」的判断成立，条件格式统计一直是空的——换文件后色阶、数据条、
图标全都出不来。修法是在 `OpenMsg::Loaded` 里把 `cf_stats_gen` 置 `u64::MAX`。

### v1.17 VBA 宏保留 ✅ 2026-11-06

不执行宏，但不能把宏弄丢：umya 写出的永远是普通 xlsx，`vbaProject.bin`、
macroEnabled 的 content type、workbook 关系全都没了。

- 保存前从基准文件里读出 `xl/vbaProject.bin`；
- 保存后塞回部件，补 `[Content_Types]` 的 Override，把 workbook 类型换成
  `application/vnd.ms-excel.sheet.macroEnabled.main+xml`，再补 workbook rels；
- `Workbook.has_vba` 供状态栏显示「含宏（保留不执行）」。
- 测试造了一个带假 vbaProject 的 .xlsm，走一遍「读 → 改 → 存」，
  宏部件、content type、改后的单元格三者都在。

### v1.18 并排比较 ✅ 2026-11-06

- `draw_grid` 拆成 `draw_grid_pane(ui, ctx, readonly)`，外层 `draw_grid` 负责分栏：
  没有比较对象时整幅画，有就左右各半（中间分隔条）。
- 「视图 → 并排比较…」选第二个文件；右侧面板**只读**：点击、拖选、下拉箭头、
  单元格编辑全部屏蔽。
- 右侧画之前把 `wb / sheet_idx / scroll / sel` 临时换过去，画完再换回来——
  用 `mem::replace` 搬移，不克隆工作簿（大文件克隆一帧都受不了）。
- 「同步纵向滚动」勾上时右侧跟随左侧的滚动量。
- 验证靠 `SC_DUMP` 的整帧累积：一帧里两个面板的几何都落盘，
  左 `rect=(280,67,776,778)` 出「产品」、右 `rect=(798,67,1294,778)` 出「姓名」。

**踩坑**：`dump_once` 原来在面板内部就置 false，第二个面板永远不 dump；
改成面板只往 `dump_accum` 追加，由外层 `flush_dump()` 统一写盘。另外加了
`open_rx.is_none()` 条件，否则异步加载还没完成就 dump 了空表。

### v1.19 真 pivotTable ✅ 2026-11-06

`crates/core/src/pivot_xml.rs`。前面 v1.15 的透视是「值化」，这一版补上真正的
OOXML 透视表，Excel 里可以拖字段、刷新。

四个部件：
- `xl/pivotCache/pivotCacheDefinitionN.xml`：`cacheSource` 指向源区域（`A1:C6` + 表名），
  `cacheFields` 逐列写，数值列带 `minValue/maxValue`，`refreshOnLoad="1"`；
- `xl/pivotCache/pivotCacheRecordsN.xml` + 它的 rels（空记录，交给 Excel 刷新时生成）；
- `xl/pivotTables/pivotTableN.xml`：`location` 是输出矩形，`pivotFields` 标出
  `axisRow`/`axisCol`/`dataField`，`rowItems` 含 `t="grand"` 总计行，
  `dataFields` 名字用 Excel 的「求和项:金额」格式；
- 关系与内容类型：workbook 的 `<pivotCaches>`（插在 `<calcPr>` 之前，符合 schema 顺序）、
  workbook rels、工作表 rels、`[Content_Types]` 三条 Override。

工作表的 rels 文件原本可能不存在（umya 生成的文件就没有），这种情况直接新建一个，
否则 Excel 不会把这个 pivotTable 认到某张表上。

`Workbook.pivots_pending` 里存待挂的透视表，`xlsx::save` 写完主体后逐个 attach；
对话框里有「生成可刷新的透视表」开关，关掉就只粘贴数值。

验证：生成的 `pivot_real_*.xlsx` 四个部件齐全、XML 全部可解析、`cacheSource` 的
`ref="A1:C6" sheet="表格"` 正确、`location ref="H1:I5"`、`dataField name="求和项:金额"`；
openpyxl 能打开且 H1/H2 读到「部门/研发」；我们自己的应用也能打开并画出这块数据。

### v1.14 图表 ✅ 2026-11-06

- 读：sheet 的 rels → `<drawing r:id>` → `xl/drawings/drawingN.xml`，
  anchor 支持 `twoCellAnchor`（from/to）和 `oneCellAnchor`（from + ext，
  EMU ÷ 9525 换算成像素再折成行列跨度）；drawing 的 rels 再定位 `chartN.xml`。
- 解析：柱状/条形/折线/面积/饼/圆环/散点，标题、图例、系列名、分类、数值。
  openpyxl 生成的图表只有 `<c:f>` 区间引用、没有缓存值时，按引用回表里读值。
- 画：`chart_view.rs` 负责绘制——标题居中、图例色块+名字、纵轴 4 档刻度、
  横轴分类标签（超过 12 个自动抽稀）、柱/线/面积/饼（圆环带内孔）。
- 插入：`chart_write.rs` 从选区生成柱状图（首行系列名、首列分类），
  新建 `chartN.xml` + `drawingN.xml` + 两处 rels + `[Content_Types]` 条目，
  并在 sheet 里挂 `<drawing r:id>`；工作表原本没有 rels 文件时自动新建。
- 验证：`chart.xlsx`（openpyxl 柱状+饼图）读出 2 张图、锚点 (1,4)-(14,9)/(19,4)-(32,9)、
  系列值与类别齐全；插入的图表用 openpyxl 复核可识别（BarChart、2 系列）。

**踩坑**：`extract_cell` 取 `<from>` 内容时用 `find('<')` 当结束位置，
结果切到第一个子元素 `<col>` 上，body 为空；要按 `</from>` 配对。
另外 `"f"` 的 End 分支里同时写了 `in_f = false` 和记录引用两个 arm，
前一个把后一个挡住了，导致引用永远为空。

### v1.15 数据透视表 ✅ 2026-11-06

`crates/core/src/pivot.rs` + 插入菜单对话框。

- 归集：按行字段分组，可选列字段交叉，值字段做求和/计数/平均/最大/最小，
  末行自动加「总计」。
- 输出：写成普通单元格块（Excel 的「粘贴为值」），任何工具都能读到数字。
- 界面：源区域取「当前区域」，行/列/值字段按表头名字选，汇总方式五种，输出到当前选中格。
- 取舍：不生成真正的 pivotTable 部件（需要 pivotCacheDefinition + pivotTableDefinition
  两套结构），因此结果不能在 Excel 里拖字段重组——这是刻意的，投入产出不划算。
- 验证：`table.xlsx` 按部门透视，研发 500、销售 500、行政 50、总计 1050；
  计数/平均也核对过；写回并存盘后再读，G1 起的那块数据完整。

### v1.13 LET 与嵌套 LAMBDA ✅ 2026-11-06

`FValue` 增加 `Lambda(Rc<LambdaVal>)`，函数成了一等公民：

- `LET(f,LAMBDA(x,x+1),f(1))` = 2；`LET(add,LAMBDA(a,b,a+b),add(3,4))` = 7。
- 词法闭包：`LambdaVal.env` 保存定义时的环境快照，所以
  `LET(mk,LAMBDA(a,LAMBDA(b,a+b)),LET(f,mk(2),f(3)))` = 5，内层函数能看见外层参数。
- 函数可以当参数传：`LET(apply,LAMBDA(g,v,g(v)),apply(LAMBDA(x,x*x),9))` = 81。
- 解析器会把函数名大写，所以环境查找按大小写不敏感匹配。
- 数组结果参与聚合时摊平：`SUM(MAP({1,2,3},LAMBDA(x,x*2)))` = 12（之前只取第一个元素）。
- 直接算一个没调用的 LAMBDA 得到函数值，显示 `#FUNC!`（Excel 是 `#CALC!`）。

**踩坑**：`Node::Func(name,args)` 的通用分支没有做环境查找，第一版只改了 LET 绑定，
`f(1)` 依然报 `#NAME?`——必须在通用调用分支最前面拦一道。

### v1.12 排序与筛选 ✅ 2026-11-06

`crates/core/src/sort_filter.rs`。

- **排序**：整行搬运（值、公式、样式一起走），空格永远排最后；数字按大小、文本按不区分大小写的字典序、
  布尔按真假。区域里有合并单元格时拒绝排序（Excel 同样拒绝）。
  「当前区域」按 Excel 规矩向外扩：上下左右碰到整行/整列空白就停。
  第一行全是非空文本且下面有数字时判定为表头，只排序数据行。
- **筛选**：不删数据，把不匹配的行隐藏（行高 0，和 Excel 一致）。表头每列一个下拉箭头，
  弹出该列所有去重值，勾选后应用；`清除筛选` 恢复。
- **存**：`<autoFilter ref="A1:C6"><filterColumn colId="1"><filters><filter val="研发"/>`，
  隐藏行写 `<row hidden="1">`（自闭合标签也能正确插属性）。

**踩坑**：
1. 排序第一版把备份按排序后的顺序做，写回等于没动——必须按**原始行序**备份，再按排序结果落到新位置。
2. `<row r="7"/>` 自闭合时属性要插在 `/` 前面，否则生成 `<row r="7"/ hidden="1">` 这种非法 XML。
3. 隐藏行只把高度设 0 还不够，绘制循环得跳过 0 高度行，否则文字会叠到下一行上。

**验证**：`table.xlsx`（5 行 3 列）排序后 `赵六/李四/王五/张三/钱七`（按金额升序），降序反过来且表头不动；
按部门筛选「研发」后隐藏 3 行，另存后 openpyxl 复核 `autoFilter A1:C6`、`colId=1 val=研发`、隐藏行 3/5/6；
几何导出确认只剩张三、王五两行，行距 22px（隐藏行占 0）。

至此这五项（条件格式、数据验证、批注、冻结窗格、排序筛选）全部落地，测试 **130+**。

### v1.11 冻结窗格 ✅ 2026-11-06

- 读：`<sheetView><pane xSplit/ySplit state="frozen"/>`；只认 `frozen`/`frozenSplit`，
  普通拆分窗格（split）不当作冻结。
- 存：`patch.rs::set_freeze_xml` 改 `<sheetView>`——自闭合形式展开成带 `<pane>` 的子节点，
  取消冻结就是删掉 pane。注意别把 `<sheetViews>` 当成 `<sheetView>`（差一个 s）。
- 画：整个绘制拆成四块——主体（行≥fy、列≥fx）、冻结列（列<fx，横向不动）、
  冻结行（行<fy，纵向不动）、左上角（都不动）。每块各自 `with_clip_rect`，
  单元格绘制抽成 `paint_cell()` 复用，批注红三角、条件格式、边框一视同仁。
- 表头跟着冻结：冻结列的列字母、冻结行的行号固定不滚；冻结边界画一条深一点的线区分。
- 交互：视图菜单 → 冻结首行 / 冻结首列 / 冻结到当前单元格 / 取消冻结；点击命中会先判断
  落在冻结区还是主体区，再换算行列。

**验证**：`freeze.xlsx`（openpyxl `freeze_panes="C2"`）读回 `(1, 2)`；
改成 `(3,1)` 另存后读回一致；再取消冻结另存，读回 `None`。
几何导出：`freeze rows=1 cols=2 body=(540,111)`，冻结标题「姓名」画在 y=100 固定带内，
主体首格 `(1,2)` 落在 y=144，两列宽的偏移量 208px 与列宽一致。

### v1.10 批注 ✅ 2026-11-06

`crates/core/src/notes.rs`（读取）、umya 回写（它自带 comments + vml 绘图生成）。

- 读：`sheet.xml` 里没有批注内容，要经 worksheet 的 rels 找到 `comments*.xml`，
  作者按 `<authors>` + `authorId` 索引；富文本多段 `<t>` 要拼接。openpyxl 写在
  `xl/comments/comment1.xml`，Excel 写在 `xl/comments1.xml`，两种路径都靠 rels 定位，不猜文件名。
- 写：`notes_dirty` 时整表重写（先清空 umya 的 comments 再灌），新增/修改/删除都生效。
- 界面：有批注的格子右上角红三角；鼠标悬停弹出「单元格 + 作者 + 内容」；
  「审阅」菜单新建/编辑/删除，编辑窗口是多行文本框，清空文本等于删除。
- 存储：`Sheet.notes: HashMap<CellRef, Note>`，不占 `Cell` 空间。

**踩坑**：`ends_with("t")` 会把 `comment` 也当成 `<t>`（"comment" 以 t 结尾），
导致批注解析始终为 0 条；改成取本地名后精确比较。这类短标签一律按本地名比较。

**验证**：`notes.xlsx`（B2 张三、A3 李四两段文本），读到 2 条、换行保留；
新增 C5 / 修改 B2 / 删除 A3 后另存，读回三条状态正确，openpyxl 也能看到 C5 的新批注。
界面几何导出确认两个红三角与作者文字。

### v1.9 数据验证 ✅ 2026-11-06

`crates/core/src/dv.rs`（模型 + 校验 + 解析）、`patch.rs`（保存）。

- 类型：整数 / 小数 / 序列 / 日期 / 时间 / 文本长度 / 自定义公式，运算符 8 种（介于、不介于…）。
- 序列来源支持内联 `甲,乙,丙` 与区域引用 `$A$1:$A$5`。
- 校验语义对齐 Excel：整型拒绝小数；空值受「忽略空值」控制；自定义公式走完整公式引擎。
- 保存：umya 的 dataValidations 处理不全，同样走 XML 补丁（剥旧块 → 按 schema 顺序插回）。
- 界面：「数据验证」菜单 → 设置对话框（类型/条件/上下限/来源/忽略空值/出错标题与说明）；
  选中带序列验证的格子时右侧出现下拉箭头，点开就是选项列表；提交不合规值时**拒绝写入**，
  保留编辑框并提示原因（对应 Excel 的「重试」）。

**踩坑**：openpyxl 把公式写成子元素 `<formula1>…</formula1>` 而不是属性，第一版只读属性导致全部读空；
另外 `showDropDown="1"` 在 OOXML 里表示**隐藏**箭头，语义和字面相反。
属性里的 `&quot;` 也需要反转义，统一走 `model::xml_unescape`。

**验证**：`dv.xlsx`（序列 C2:C20、整数 1-100 B2:B20 带自定义错误提示）。读回 2 条、C2 选项 4 个、
B2 校验 50 通过 / 0 与 101 与 3.5 拒绝；另存后 openpyxl 复核 3 条验证齐全（含新增的 textLength）。
界面几何导出确认 C2 的下拉箭头画在 (628,122)、选项数 4。

### v1.8 条件格式 ✅ 2026-11-06

`crates/core/src/cf.rs`（模型 + 解析 + 求值）、`crates/core/src/patch.rs`（保存时补 XML）。

- 读：`<conditionalFormatting>` + `styles.xml` 的 `<dxfs>`。支持比较类、包含文本、
  重复/唯一值、前/后 N 项、高于/低于平均值、公式规则、色阶、数据条。
- 算：`overlay_at()` 按块求叠加（填充/字色/加粗/数据条比例），色阶按块内 min/max 插值；
  公式规则的相对引用按**块左上角**平移（Excel 语义），走完整公式引擎。
- 画：填充画在底色之上、文字之下；数据条按比例画半透明条；字色/加粗按叠加生效。
- 改：工具栏「条件格式」菜单——大于/小于/等于/介于、前后 10 项、平均值、数据条 4 色、
  色阶 3 套、清除所选范围。
- 存：umya 的 dxfId 处理不可靠，改成保存后重写 zip：
  剥掉原有 `<conditionalFormatting>` 再按 schema 顺序插回（mergeCells 之后、dataValidations 之前），
  同时整体替换 `styles.xml` 的 `<dxfs>`，索引对得上。

**验证**：`cf.xlsx`（openpyxl 生成：大于100红底、三色色阶、数据条、包含"急"黄底）。
无界面几何导出显示：B3=180 命中 `#FFC7CE`、B5=320 数据条宽度 104px（满格）、
C2/C3/C4 色阶分别取红/插值黄/绿、D3/D4 命中黄底、D2/D5 不命中。
改完保存后 openpyxl 复核：5 条规则全在，新增的 containsText 规则 dxfId=2 正确。

### v1.7 列宽行高 + 格式工具栏 + 边框 ✅ 2026-11-06

**列宽行高**（`crates/core/src/layout.rs`）
- `Axis`：默认值 + 只记被改过的项，位置查询走 `BTreeMap` 累计偏移 + 二分，O(log n)。
- 换算：列宽 = `字符数 × 7 + 5`（Excel 8.43 字符 → 64 px），行高 = `pt × 96/72`。
- 只记 `customWidth="1"` 或宽度≠8.43 的列，避免 `min=1 max=16384` 这种声明把整张表铺开。
- 渲染、点击命中、滚动定位、`ensure_visible` 全部改成按实际尺寸算，不再是「等宽 × 索引」。
- 实测（样式测试表）：A=6字符→47px、B=12→89、C=14→103、D=26→187、E=10→75、F=16→117；
  行高 1 行 40pt→53px、2 行 26pt→35px；命中测试全对。

**格式工具栏**（第二行）
加粗 / 斜体 / 字号 / 字色 / 填充色 / 无填充 / 左中右对齐 / 数字格式（常规·数值·货币·百分比·日期）/ 边框 / 合并 / 拆分。
- 支持**框选**（拖拽建立选区，底色 + 边框标示），所有操作作用于整个选区。
- 改样式只重算显示，不触发公式重算；单次上限 20 万格，超过直接拒绝并提示。

**边框**
- 模型：每格四边 `Border{style, color}`，0/1/2/3/4/5/6 = 无/细/中/粗/双线/虚线/点线。
- 渲染：双线画两条、虚线点线按间距切段，合并区域按整块画外框。
- 工具栏：「无边框 / 全部框线 / 外边框 / 粗外框」。

**保存只回写改动**
- `Sheet.style_edits` 记录用户改过样式的格子，`merges_dirty` 记录合并是否被改；保存时只回写这些，
  没动过的文件保持零额外开销（10 万格级别不至于因为写样式拖慢）。

**踩的坑**：合并区域相交判定把行列写反（`*d < r1` 应为 `*cc < r1`），
表现是「取消合并」调用了但保存后合并还在。测试 `edit_style_merge_border_then_save` 抓到的。

**验证**
- 88 个测试通过（新增 layout 5 个、格式编辑 3 个、合并冲突、边框往返）。
- 保存后用 openpyxl 复核：新合并 `B13:C14` 写入、`A8:C8` 已拆、`B10` 黄底加粗细边框 `#,##0.00` 数值都在。
- 无界面几何导出（`SC_DUMP`）：合并标题块 `(332,89)-(950,142)`，宽 618px 正好等于 6 列宽之和，
  高 53px 等于 40pt 行高；边框 medium 正确落到 4 条边。

### v1.6 样式与合并单元格 ✅ 2026-11-06

原生模型不再只是「值 + 公式」：每个单元格可以带样式索引，每张表带合并区域。

**数据结构**
- `crates/core/src/style.rs`：`CellStyle{bold, italic, size, color, fill, halign, valign, wrap, format}`；
  `StyleTable::intern()` 按内容去重，每格只存 `u16` 索引。测试文件 34 个带样式格压成 **12 条样式**。
- `crates/core/src/merges.rs`：直接解析 `xl/worksheets/*.xml` 的 `<mergeCells>`。
  表名 → 文件靠 `xl/workbook.xml` 的 `r:id` + `xl/_rels/workbook.xml.rels` 定位，不靠顺序猜。
- `Sheet.styles: HashMap<CellRef, u16>` **稀疏存放**——没样式的格子零开销，不改 `Cell` 结构（40 字节不变）。
- `Sheet.merges: Vec<(r1,c1,r2,c2)>`。

**数字格式** `style::format_value()`：百分比、千分位、货币前缀、小数位、`yyyy-mm-dd` 与 `h:mm AM/PM`。
未覆盖的格式回退到常规显示，不报错。

**渲染**
- 底色、字色、字号、加粗、水平/垂直对齐。
- 合并区域按整块尺寸绘制（`rect.set_width(COL_W * 列数)`），非锚点格跳过，点击落在区域内自动选中左上角，选区边框也按整块画。

**两个真 bug（都修了）**
1. **另存会丢合并单元格和样式**：保存时用「目标路径」当基准文件，另存到新路径时目标不存在 →
   umya 从零建表 → 只剩单元格值。改成用 `wb.path`（打开时的原文件）当基准。
   验证：另存后 openpyxl 读回 `A1:F1`/`A8:C8`/`D2:F6` 等合并区域与字体填充格式全部还在。
2. **选中色把填充色染偏**：`rgba(26,115,232,40)` 画在填充之上，深蓝 `#1F4E78` 被染成 `#1F70C9`，
   纯红 `#FF0000` 变成 `(220,46,100)`。改成有填充时不画选中底色（只留边框）。

**代价**：另存到新路径现在要读一遍原文件（152 MB 多约 2.3 s），这是保真的必要成本；
原地保存不变。实测大文件：打开 0.40 s、重算 11.8 s、另存 3.9 s、峰值 490 MB。

**测试 79 个**（新增样式去重、数字格式、合并区域解析、另存保真回归）。
截图证据：`work/ui_v16_styles.png`、`work/ui_v16_merges.png`。

### v1.5 分页列式存储 ✅ 2026-11-06

`crates/core/src/store.rs`：页高 256 行，页内按列存放 `Vec<Cell>`，空单元格不分配。

| 指标 | HashMap | 行内向量 | **分页列式** |
|---|---|---|---|
| 内存（100 万格） | 124 B/格 | 111 B/格 | **64 B/格** |
| 152MB 全量重算 | 19.11 s | 14.07 s | **14.52 s** |
| 另存 | 2.03 s | 2.03 s | **1.54 s** |
| 编辑一格（连带 1.5 万格） | 60 ms | 69 ms | **64 ms** |
| GUI 工作集 | 182–207 MB | 194 MB | **179 MB** |
| 测试 | 69 | 69 | **73** |

比最初版本内存降 **48%**，重算快 **24%**，另存快 **24%**。

**走过的一个弯路**：为了补上列扫描的哈希开销，给 `cached_grid` 加了「单列整段取出」的快路径，
结果重算从 16.0 s 变成 **19.8 s**——因为公式列（金额 = 数量×单价）每格都要回表求值，
快路径多了一次 `Vec<&Cell>` 收集和一次遍历，纯属倒忙。已回退。

**稀疏保护**：页只按实际用到的行数分配（`Page.n_rows`），
所以「100 万行里只有 3 格」这种表不会按整页 256 行分配。有测试覆盖。

### v1.4 存储层紧凑化（行内有序向量） ✅ 2026-11-06

**先拆账再动手**：124 字节/格里只有 40 是 `Cell` 本体，约 56 是 hashbrown 槽位开销
（每条目 48 字节 + 控制字节 + 容量余量），剩下才是字符串分配。

所以第一刀砍的是存储结构，不是字符串：

`HashMap<CellRef, Cell>` → `HashMap<u32 行号, Vec<(u32 列号, Cell)>>`（行内按列升序，二分查找）

| 指标 | 之前 | 现在 |
|---|---|---|
| 内存（100 万格混合负载） | 124 字节/格 | **111 字节/格** |
| 152MB 文件全量重算 | 19.1 s | **14.1 s**（−26%） |
| 编辑一格（15k 格连带重算） | 60 ms | 69 ms |
| GUI 工作集 | 182–207 MB | **194 MB** |

重算变快是因为范围扫描变成了连续内存遍历（SUM 一个 1.5 万格区域不再跳哈希表）。

**为什么只降了 13 字节**：每行一个 `Vec` 意味着每行一次分配 + 容量余量（5 个格子通常预留 8 个），
再加上 `HashMap<u32, Vec>` 每行一个条目。实测下来这部分抵消了大半收益。

**结论**：字符串 arena 的收益比我先前估的小（40→32 字节/格 + 省掉 malloc 头，合计约 15–18 字节/格）。
要真正到 60 字节/格，需要**分页列式存储**（所有单元格放在大块 `Vec<Cell>` 里、按列分块），
那是存储层重写，不是加个 arena 能解决的。

### v1.3 入 git + 单元格模型紧凑化 ✅ 2026-11-06

**仓库**：`<workspace>` 已 `git init` 并推送到
https://github.com/ChroniCat/finance-collab-site 的 `native-rust-sheets` 分支
（该仓库默认分支是 `codex/finance-web-mvp`，是另一套 Web 实现，所以没动主干）。
`.gitignore` 排除 `vendor/`、`vendor-web/`、`site*/`、`.local/`（19 GB）、`native/target/`（15 GB）、`work/`。

**模型紧凑化第一步**：
| 类型 | 之前 | 现在 |
|---|---|---|
| `Value` | 32 字节（String 24 + tag） | **24 字节**（Box<str> 16 + tag） |
| `Cell` | 56 字节 | **40 字节** |

实测（100 万格：数字/文本/公式混合）：私有内存 +118 MB = **124 字节/格**。
其中 40 字节是 Cell 本体，其余是 HashMap 条目（约 16 字节/条）与字符串堆分配。

**下一步（未做）**：真正的紧凑化需要字符串 arena + 16 字节 Cell：
`{ kind: u8, num: f64, idx: u32 }`，文本与公式都存进每表一个 `Vec<u8>`。
预计能到 **60–70 字节/格**，但要改 model、xlsx 读写、公式解析、协作序列化四处，风险高。

### v1.2 多表协作 + 异步保存 ✅ 2026-11-06

**多表协作**：`Op` 和 `Cursor` 增加 `sheet` 字段，冲突键从 `(row,col)` 改成 `(sheet,row,col)`。
- 宿主快照改成遍历所有工作表（原来只塞当前表）
- 客户端按 op 里的表号写回对应工作表
- 实测：宿主打开 152MB/6 表文件 → 客户端收到 **431,652 条操作**（修之前只有 160,008，即当前一张表）
- 新增测试：同 (row,col) 不同表不冲突；同表同格仍按 (ts,site) 收敛

**保存路径**：先量了一下 152MB 文件（431,652 格）的保存耗时：

| 阶段 | 耗时 |
|---|---|
| 读入原文件（保留样式的前提） | 0.72 s |
| 组装 umya DOM | 0.42 s |
| 序列化 + 压缩 | 0.59 s |
| 合计 | ~1.7 s |

250 万格的文件是 3.86 s + 4.58 s = 9.94 s。

结论：**没有重写 writer**。原因——重新生成 sheet XML 会丢掉合并单元格、条件格式、数据验证、批注、图片锚点
（这些全在 `<sheetData>` 之外的同一份 XML 里）；要保留就得做「只替换改动单元格」的 XML 级补丁，
工作量和风险都远超收益（对真实文件 1.7 s 已经可接受）。

实际做的是**异步保存**：克隆一份工作簿到后台线程 → 写 `.tmp` → 成功后 `rename` 原子替换。
保存期间界面不卡，状态栏显示「正在保存…/已保存」。

**意外事故与修复**：打异步保存补丁时，替换区间算错，把 `commit_edit` / `handle_keys` / `draw_grid`
三个函数误删了（约 150 行）。已重写，并用截图验证界面正常：表格、行列表头、选区、公式栏、
6 张表标签、文件树都在。教训：改 Rust 源码必须按函数边界定位，不能用「从 A 到 B」的粗略切片。

**测试** 67 → **69**（新增多表不冲突、保存读回一致性）

### v1.1 协作增量重算 ✅ 2026-11-06

**问题**：协作收到远端操作时，`poll_net` 里用「远端光标位置」当种子，取不到就退化成 `recalc_all` + 重建依赖图 —— 一次编辑卡 17 秒。

**改动**：
1. `sync::Peer::apply_list_to_sheet`：只写这批被接受的操作所涉及的格子，不再整表重写（原来每条 op 都调用 `apply_to_sheet` 全表刷一遍）
2. `deps::Deps::update_cell`：新增 `cols_of` 记录每个公式登记过的列，删除时精确移除（原来是 `by_col.retain` 扫全部列索引）
3. `poll_net`：按**实际被接受的 op** 收集 `touched`，只对这些格子做 `update_cell` + `recalc_dirty`
4. 只有「加入房间时收到的第一包全量快照」才走全量重算 + 重建依赖图

**实测**（real100.xlsx，152 MB，22.6 万公式）：

| 远端批量 | 重算格数 | 耗时 | 对比全量 |
|---|---|---|---|
| 50 条 | 15,098 | 57 ms | 17,000 ms |
| 500 条 | 15,998 | 66 ms | 17,000 ms |
| 5000 条 | 24,998 | 88 ms | 17,000 ms |

**顺带修的回归**：打开改成异步后，`--host` 在文件还没读进来时就执行了，宿主快照是空的，
客户端只收到 `ops: 0`。现在把共享/加入动作挂起到 `Loaded` 之后再执行。
实测：宿主 43 万格文件 → 客户端收到 160,008 条操作（当前只同步打开的那张表）。

**已知限制**：协作只同步当前打开的那张工作表，多表文档的其他表不同步。

### v1.0 打开不再卡界面 ✅ 2026-11-06

`open_path` 原来在 UI 线程里同步 `load` + `recalc_all`，打开 152MB 文件要卡 18 秒白屏。
改成两段式：

1. 后台线程 `load` → 把读到的表发给 UI（带的是 Excel 上次保存的计算结果）→ 界面立刻可画
2. 同一线程继续 `recalc_all` + 建依赖图 → 算完再整体换掉

细节：
- `doc_gen` 代次号：后台算完时如果用户已经改过格子，结果**丢弃**，避免覆盖用户输入
- 丢弃后不立刻重算（否则连续输入会反复克隆整本工作簿），而是标记 `needs_full`，
  等用户停手 3 秒再补一次
- 后台任务期间每 100ms 重绘，状态栏显示进度

**实测**（152MB / 431,652 格 / 226,036 条公式）：

| 指标 | 之前 | 现在 |
|---|---|---|
| 首屏可用 | 18–20 s（白屏） | **1.22 s** |
| 后台重算完成 | — | 17.2 s |
| 稳态工作集 | 174.7 MB | 182.3 MB |
| 峰值（重算时两份副本） | — | ~577 MB |

代价：后台重算期间内存翻倍（要克隆一份工作簿给子线程）。用 400MB 换 17 秒白屏，值得。

### v0.9 增量重算 ✅ 2026-11-06

**问题**：`commit_edit` 每次编辑都调 `recalc_all`，在 22.6 万条公式的表里等于每次回车卡 20 秒。

**做法**：
1. 打开文件时建依赖图（`formula::deps::Deps`）：每条公式记录它引用的矩形区域
   - 不展开成逐格边（A1:A20000 会变成 2 万条边 × 22 万公式），只存矩形
   - 另建「列号 → 公式列表」索引，某格变化时只查这一列
2. 编辑后 `recalc_dirty(wb, deps, seeds)`：求出依赖闭包，只重算这些格子
3. `Calc.scope`：不在本次范围内的公式单元格直接返回缓存值，不再求值
4. 不需要拓扑排序——求值时递归拉取依赖，memo 保证每格只算一次

**实测**（`work/stress/real100.xlsx`，152 MB / 43 万格 / 22.6 万公式）：

| 操作 | 耗时 |
|---|---|
| 打开 + 首次全量重算 | 18.4 s（一次性） |
| 建依赖图（126,011 条公式） | 0.35 s |
| 改「明细!D」一行（触发 15,002 格重算） | **60 ms** |
| 改「基础数据!D101」（只影响 1 格） | **0.2 ms** |
| 无依赖的格子（表头） | 0.0 ms |

**正确性**：改完 21 处后，把增量结果与「整表全量重算」逐格对比 → **0 处差异**。

**遗留**：
- 打开文件仍是同步重算（18 秒卡住窗口），需要改成后台算 + 进度提示
- 依赖图在协作收到远端批量操作时没有增量更新，会退化成全量重建

### v0.8 真实大文件压测 + 重算提速 ✅ 2026-11-06

**测试文件** `work/stress/real100.xlsx`（openpyxl 生成，模拟真实业务表）：
- 6 张表：基础数据(2万行×8) / 明细(1.5万行×10) / 汇总(500行) / 历史(2万行×5) / 透视(5000行×4) / 图片
- 431,652 个单元格，**226,036 条公式**（VLOOKUP/SUMIF/SUMIFS/COUNTIFS/INDEX+MATCH/SUMPRODUCT/跨表引用）
- 70 张 1600×1200 高熵 JPEG，文件 **152.7 MB**

**结果**（release，单线程）：

| 阶段 | 优化前 | 优化后 |
|---|---|---|
| 打开 | 0.29 s | 0.29 s |
| 重算 | **115.4 s** | **19.1 s** |
| 另存 | 1.9 s | 1.9 s |
| CLI 峰值内存 | 233 MB | 481 MB |
| GUI 工作集 | 138 MB | 138 MB |

**做了什么**：
1. 整列引用（`基础数据!A:A`）按表的实际使用范围裁剪，不再展开 104 万空单元格
2. `Arg::Range` 改为 `Rc<Vec<FValue>>`，同一区域一轮重算只物化一次
3. 区域缓存加 LRU（上限 1600 万格 ≈ 400 MB），避免逐行累积区间把内存吃穿
   - 无上限时：23 s / 4.6 GB；加上限后：19 s / 481 MB
4. 条件聚合（SUMIF/COUNTIF）的 criteria 预编译，逐格比较不再 `display().to_lowercase()`
5. 精确匹配的 VLOOKUP/MATCH 建哈希索引，O(n) → O(1)
6. 区域聚合结果缓存（SUM/SUMIF/COUNTIF/COUNTA/AVERAGE/MAX/MIN）

**压测暴露的两个体验问题（尚未修）**：
- `open_path` 同步调用 `recalc_all`，打开这个 152MB 文件时窗口会卡 19 秒
- **每次编辑单元格都触发全量重算**（`commit_edit` 里直接 `recalc_all`），在 22.6 万条公式的表里等于每次回车卡 19 秒
- 需要做增量重算（脏区 + 依赖追踪），否则大文件基本没法编辑

### v0.7 函数补完 + 大文件压测 ✅ 2026-11-06
- 函数 463 → **475**：新增 ODDFPRICE/ODDFYIELD/ODDLPRICE/ODDLYIELD（不规则首末期）、
  BESSELI/J/K/Y（J/I 走级数，K/Y 走积分表示 + Simpson 求积）、
  FORECAST.ETS 家族（ETS/CONFINT/SEASONALITY/STAT，Holt-Winters + 网格搜索平滑系数）
- 测试 60 → **62**
- 剩余 8 个函数需要外部世界，暂不做：WEBSERVICE、FILTERXML、IMAGE、STOCKHISTORY、RTD、GETPIVOTDATA、FIELDVALUE、BAHTTEXT
- **大文件压测**（release，单线程）：

| 文件 | 单元格 | 打开 | 重算 | 另存 | CLI 峰值内存 | GUI 工作集 |
|---|---|---|---|---|---|---|
| 5 万行 | 25 万 | 0.13 s | — | 0.69 s | 133 MB | — |
| 20 万行 | 100 万 | 0.52 s | 0.01 s | 3.21 s | 471 MB | 208 MB |
| 10 万行 / 20 万公式 | 60 万 | 0.36 s | 0.76 s | 1.85 s | 312 MB | — |
| 50 万行 | 250 万 | 1.29 s | 0.03 s | 10.57 s | 1082 MB | 321 MB |

- **瓶颈**：另存（10.57 s / 250 万格）和 CLI 峰值内存（1 GB）。原因：`xlsx::save` 走 umya-spreadsheet，
  要在内存里重建整棵 DOM 再序列化。GUI 的工作集低得多（321 MB），因为它提前释放了 umya 的中间结构。
- 下一步优化方向：自己写流式 sheet XML + zip，跳过 umya 的 DOM

### v0.6 函数扩容 + 查找性能修复 ✅ 2026-11-06
- 函数 419 → **463**
- 新增：债券（PRICE/YIELD/DISC/INTRATE/RECEIVED/PRICEDISC/YIELDDISC/PRICEMAT/YIELDMAT）、
  票息日（COUPNUM/COUPPCD/COUPNCD/COUPDAYS/COUPDAYBS/COUPDAYSNC）、
  正则（REGEXTEST/REGEXEXTRACT/REGEXREPLACE，走 Rust regex crate）、
  国际化工作日（NETWORKDAYS.INTL/WORKDAY.INTL，支持 1-17 周末码与 7 位字符串掩码）、
  矩阵（MDETERM/MINVERSE）、财务补充（COVAR/PERCENTOF/LOGEST/BINOM.DIST.RANGE）、
  数学（GAMMA/GAUSS/PHI/ACOTH/PERMUTATIONA/TDIST）、文本（CLEAN/ASC/DOLLAR/FIXED/NUMBERVALUE）、
  日期（DATEVALUE/TIMEVALUE/DAYS360/ISOWEEKNUM）、信息（CELL/AREAS/INFO）
- **性能修复**：VLOOKUP/HLOOKUP/MATCH/XMATCH/XLOOKUP 改为「区域只物化一次 + 直接读结果格」
  - 1 万行 × 1 万次查找：37.68 s → **0.27 s**（140×），265 格/秒 → 36,698 格/秒
- 顺带修：`INDIRECT` 返回的数组恒为 1×N（丢失行列），`range_dims/range_get` 不认数组参数，
  导致 `VLOOKUP(x,INDIRECT("A1:B9"),2,FALSE)` 报 #REF!
- 基准（release，单线程）：
  - 简单引用 10 万格：0.29 s（34 万格/秒）
  - SUM 滚动窗口 2 万格：0.14 s（14 万格/秒）
  - VLOOKUP 1 万次：0.27 s（3.7 万格/秒）
- 测试 57 → **60**

### v0.5 公式扩充 + 协作加固 ✅ 2026-11-06
- 函数 363 → **419**，覆盖 Excel 365 约 **85%**
- 新增：假设检验（T.TEST/F.TEST/CHISQ.TEST）、回归（RSQ/FORECAST/STEYX/TREND/GROWTH/LINEST）、
  百分位补充（PERCENTRANK/PERCENTILE.EXC/QUARTILE.EXC/RANK.AVG）、CONVERT（长度/质量/时间/体积/面积/温度）、
  财务（VDB/DURATION/MDURATION/TBILLPRICE/TBILLYIELD/TBILLEQ/AMORLINC/AMORDEGRC）、
  条件聚合（AVERAGEIFS/AGGREGATE/FREQUENCY/PROB/STDEVA/VARA/MODE.SNGL/MODE.MULT/COVARIANCE/PEARSON）、
  文本数组（TEXTSPLIT/WRAPROWS/WRAPCOLS/ENCODEURL/HYPERLINK）、引用元信息（ISFORMULA/FORMULATEXT/SHEET/SHEETS）、
  引用运算（INDIRECT/OFFSET/ADDRESS，修正 ROW()/COLUMN() 无参返回当前格）、
  **LAMBDA 家族**（LAMBDA/MAP/REDUCE/SCAN/BYROW/BYCOL/MAKEARRAY/ISOMITTED）
- 数组常量 `{1,2;3,4}` 进入词法/语法层（此前 `=SUM({1,2,3})` 直接 #VALUE!）
- 测试 47 → **57**
- 已知限制：`LET(f,LAMBDA(x,x+1),f(1))` 这种「把 LAMBDA 存进名字再调用」不支持，因为函数值没有类型；
  LAMBDA 只能作为 MAP/REDUCE/BYROW 等函数的实参内联使用

### v0.4 协作 ✅ 2026-11-06
- 同步模型：每格「最后写入者胜出」，全序 = (Lamport 时间戳, 站点号)；可证明收敛
- 拓扑：星形。谁开文档谁当宿主（TCP 7878），其他人连他；宿主转发并保存全量快照
- 协议：行分隔 JSON（Hello / Welcome / Snapshot / Op / Cursor / Bye）
- 客户端：`共享` 按钮启动宿主、填写 `IP:端口` 加入；在线人数、远端光标（带颜色与名字）实时显示
- 采纳远端快照时清空本地残留，避免两边的旧内容混在一起
- 测试：5 个同步内核用例 + 2 个真实 TCP 用例（含两端各改一格后收敛）
- 双进程实测：A `--host` 启动宿主（192.0.2.232:7878），B `--join=127.0.0.1:7878` 加入，B 收到 50 条快照操作，A 记录到 `peer joined: example-user#76601`
- 修：窗口静止时不重绘会导致收不到远端更新 → 协作中每 100ms 主动重绘
- **口令**：宿主发 challenge，客户端回 sha256(nonce||口令)，口令不上线；空口令=开放
- **断线重连**：客户端监督线程指数退避（1→10 秒），重连后自动拉新快照
- 实测：宿主重启后客户端 2 次失败重试 → `ops: 50` 重新同步；错口令 `denied: 口令不正确`，对口令 `connected`

### v0.4 协作
- 宿主模式：一人开文件即监听，其他人按 IP 加入
- CRDT 同步单元格变更、光标位置、在线名单
- 断线重连、离线编辑后合并

### v0.5 打包
- Windows 安装包（MSI）、文件关联、开机自启
- 绿色版单 exe

## 明确不做的（第一年）

- VBA 宏
- 数据透视表编辑
- 图表编辑（只显示）
- Power Query
- 打印排版精确对齐

这些是 Excel 四十年积累的部分；先把“打开、编辑、协作、保存”做到可靠。

## 与 ONLYOFFICE 路线的关系

两条路并行不冲突：
- ONLYOFFICE 路线：3 周可交付，功能全，但是 Chromium
- 原生路线：从零开始，但内存小、可控、无 AGPL 约束

原生版本达到可用（v0.4）之前，ONLYOFFICE 那套可以作为内部过渡工具。

## 实测数字（v0.1，2026-11-06）

同一台机器、同一个文件（sample.xlsx，14 个非空格）、启动后静置 7 秒测量。

| 配置 | working set | private | exe |
|---|---:|---:|---:|
| wgpu + 全量中文字体（16 MB） | 308.8 MB | 353.0 MB | 14.07 MB |
| glow(OpenGL) + 全量中文字体 | 105.0 MB | 113.0 MB | 10.12 MB |
| **glow + 内嵌子集字体（2.8 MB）** | **74.0 MB** | **80.9 MB** | **12.76 MB** |

结论：
1. **后端比字体重要得多**。wgpu/DX12 在这台机器上多占 200 MB 以上（显存驻留计入 working set）；换 glow(OpenGL) 直接降到 105 MB。
2. **字体子集再省 31 MB**：用 fontTools 把 `Deng.ttf` 裁成 GB2312 常用字（7,542 字，2.78 MB）并 `include_bytes!` 进二进制，内存 105 → 74 MB，中文渲染无缺字。
3. 最终 v0.1 形态：**74 MB / 12.76 MB exe**，是同场景 wgpu 版的 1/4。

注意：glow 依赖显卡 OpenGL 驱动；在某些远程桌面/无 GPU 环境会白屏（本项目在 RDP 会话里遇到过），需要探测失败时回退 wgpu 或软件渲染。

## 与 ONLYOFFICE 桌面版对照（同机同文件）

ONLYOFFICE Desktop Editors 10.0.0（官方安装包，2026-11），打开同一个 `sample.xlsx`，静置 30 秒后测量：

| | 进程数 | working set | private |
|---|---:|---:|---:|
| ONLYOFFICE 桌面版 10.0 | 8 | **918.8 MB** | 819.6 MB |
| 本项目 原生 v0.1 | 1 | **74.0 MB** | 80.9 MB |
| 倍数 | 8× | **12.4×** | 10.1× |

同一份数据、同一个测量方式（all processes sum, working set）。原生版约为其 1/12。
