# 三项待办：实现方案（v1.47，已全部实现）

> 实现见 `docs/native-rewrite.md` 的 v1.47 一节；本文保留当时的实测与设计。

> 正文的“还没做”指当时提案状态。当前任务请以 [2026-11-12 Excel 与 WPS 对标审查](excel-wps-audit-2026-11-12/README.md) 为准，避免重复实现已有能力。

这三项都还没做。下面是先做过的实测（不是推测），以及据此定的方案。

## 0. 实测结论：三条前提要先纠正

**① 外部引用现在算不了。** `=[1]预算!A1`、`=SUM([1]预算!A1:A3)`、`=[ext_plan_src2.xlsx]预算!A1`、
`='[ext_plan_src2.xlsx]预算'!A1` 四种写法在 `eval_formula` 里分别是 #VALUE! / #VALUE! / #VALUE! / #REF!。
缓存值能读进来（A1=10、A2=60、A4=预算元），但一按「重算」就全变 #VALUE!。

**② 包结构反而已经保住了。** 拿 Excel 造的 `ext_plan_main2.xlsx`（1 个外部链接）走我们的
`load → 改一格 → save`，五项都在：`xl/externalLinks/externalLink1.xml`、它的 rels、
`workbook.xml` 的 `<externalReferences>`、`workbook.xml.rels` 的 externalLink 关系、CT 的 Override；
`<f>[1]预算!A1</f>` 也原样。Excel 打开另存后的文件：`链接=1 A1=10 A2=60 U21=probe`，公式显示为
`'<workspace>\work\[ext_plan_src2.xlsx]预算'!A1`。
→ 缺的只有「求值 + 刷新缓存 + 界面」，不是「写包」。

**③ 剪贴板的公式不止在 Embed Source 里。** 从 Excel 复制 A1:B2（含两个公式）后，剪贴板上有 21 种格式，
其中三种都能出公式：

| 格式 | 大小 | 内容 |
|---|---:|---|
| **XML Spreadsheet** | 1,318 B | `<Cell ss:Formula="=RC[-1]+R[1]C[-1]"><Data ss:Type="Number">3</Data></Cell>`，还带 FontName/Size/Bold/Interior/Borders/NumberFormat |
| Embed Source | 18,587 B | OLE 复合文档（`D0CF11E0`），Workbook 流 = BIFF8，公式是 ptg 令牌 |
| Biff12 | 6,398 B | 其实是 zip（`PK\x03\x04`），.xlsb 的二进制包 |

→ 主路走 **XML Spreadsheet**（R1C1 文本，直接可转 A1），HTML 留作兜底，BIFF 不做。
只有「源头只放 Biff、没有 XML Spreadsheet」的旧环境才需要 ptg 解码器，那种情况先记为已知限制。

## 1. 跨工作簿引用

### 文件格式（实测样本）

```
xl/worksheets/sheet1.xml:
  <c r="A1"><f>[1]预算!A1</f><v>10</v></c>
  <c r="A2"><f>SUM([1]预算!A1:A3)</f><v>60</v></c>

xl/workbook.xml:
  <externalReferences><externalReference r:id="rId2"/></externalReferences>

xl/_rels/workbook.xml.rels:
  rId2 → externalLinks/externalLink1.xml  (type: .../externalLink)

xl/externalLinks/externalLink1.xml:
  <externalBook r:id="rId1">
    <sheetNames><sheetName val="预算"/></sheetNames>
    <sheetDataSet><sheetData sheetId="0">
      <row r="1"><cell r="A1"><v>10</v></cell><cell r="B1" t="str"><v>预算</v></cell></row>
      <row r="2"><cell r="A2"><v>20</v></cell><cell r="B2"><v>30</v></cell></row>
    </sheetData></sheetDataSet>
  </externalBook>

xl/externalLinks/_rels/externalLink1.xml.rels:
  rId1 → Target="ext_plan_src2.xlsx"          TargetMode="External"   （相对）
  rId2 → Target="file:///F:\...\ext_plan_src2.xlsx" TargetMode="External"（绝对，xxl21:absoluteUrl 引用它）
```

两个要点：
- 工作表 XML 里的 `[N]` 是**链接序号（1 起）**，不是文件名；文件名只在 externalLink 的 rels 里。
- 缓存是「被引用表的已用区域」，值是 `<v>`，文本用 `t="str"`，没有公式。

### 数据模型（crates/core）

```rust
pub struct ExternalBook {
    pub index: usize,        // 1 起，对应 [N]
    pub target: String,      // 相对路径（rels rId1）
    pub abs: Option<String>, // 绝对 URL（可选）
    pub sheets: Vec<String>, // <sheetName val>
    pub cache: std::collections::BTreeMap<(usize, u32, u32), Value>, // (sheetId,row,col) → 值
    pub refreshed: bool,     // 本轮是否从磁盘重读过（决定存盘要不要重写 sheetDataSet）
}
// Workbook.links: Vec<ExternalBook>
```

### 公式链路（crates/core/src/formula）

1. 词法：`'['` 分支读到 `']'`，产出 `Tok::Book(String)`（`预算.xlsx` 或 `1`）。
2. 语法：`[..]Sheet!A1`、`[..]Sheet!A1:B2`、`'[..]Sheet'!A1` → `Node::Ref{ book: Option<usize>, sheet, row, col }`；
   能按序号（`1`）或文件名（`ext_plan_src2.xlsx`，忽略大小写、可带 `file:///`）解析到 links 下标。
3. 求值：`Node::Ref` / `Node::Range` 命中 book 时改从 `links[i].cache` 读；范围按 cache 的矩形取，
   缺的格当空。解析不到书籍 → `#REF!`。
4. **显示**：公式栏把 `[1]` 显示成 `[ext_plan_src2.xlsx]`（读时把序号换成文件名），
   **写回必须还原成 `[N]`**（Excel 只认序号形式）。这条要单测锁死。

### 更新机制

- **打开**：默认沿用文件里的缓存（和 Excel「不更新链接」一致，快且离线可用）。
- **更新链接**（数据页 → 编辑链接… → 更新）：按 target 解析真实路径（先工作簿同目录，再绝对 URL），
  用 calamine 读目标表的已用区域 → 填 `cache` → `recalc_all` → 标记 `refreshed`。
- **存盘**：`refreshed` 的表重写 `sheetDataSet`（sheetNames/rels/CT 原样）；没刷新过就一个字节都不动。
- 目标文件缺失：状态列显示「文件缺失」，求值继续用缓存，不把 #REF! 写回。

### 界面

- 数据页 →「编辑链接…」：表格（文件名 / 绝对路径 / 状态：已缓存·已刷新·缺失 / 工作表数），
  按钮：更新、全部更新、断开链接（把公式替换成当前值）。
- 状态栏：`外部链接 2 个（1 个缺失）`。

### 验收（Excel COM）

1. 打开 `ext_plan_main2.xlsx` → 重算后 A1=10、A2=60、A3=21、A4=预算元、A5=30（和 Excel 一致）。
2. 把源文件 A1 改成 100 → 点更新链接 → A1=100、A2=150（Excel 里同样操作结果一致）→ 保存。
3. 用 Excel 打开保存后的文件：链接数 1，值为新值，公式显示 `'[ext_plan_src2.xlsx]预算'!A1`。
4. 回归：不带外部引用的文件另存，包内部件与基准逐字节相同（现有测试 basis）。

## 2. 修订跟踪（单元格级）

### 先排除 Excel 的旧式修订

Excel 的 Track Changes 绑定「共享工作簿」+ `<revisions>` 部件，Excel 365 已弃用、LibreOffice 也不认。
做出来既不互操作、又难维护。**改做应用级修订日志**：能回答「谁、什么时候、改了哪格、之前是什么」，
并且能撤回——这正是用户要的「单元格级改动历史」，且复用现有撤销/协作设施。

### 切入点（代码里已经找好）

所有编辑都汇聚到 `SheetApp::txn_end(label, before)`（约 20 处调用），它已经拿到
`CellDelta { before, after, before_style, after_style }`；结构性改动走
`snap_before/snap_after` 快照。远端改动在 `Msg::Op / StyleOp / MergeOp / Structural` 的应用处。
→ 在 `txn_end` 末尾 append 一条修订；远端在应用处 append（带 site/name）。

### 存储（随文件走，不进 xlsx）

```
<工作簿同目录>/.~sheetflow-history/<文件名>/revisions.jsonl
{"ts":1762...,"who":"张三","site":0,"kind":"cell","sheet":0,"r":9,"c":1,
 "before":{"v":"10","f":null,"st":12},"after":{"v":"20","f":"=A1*2","st":13},
 "label":"编辑单元格","epoch":42}
```

- 只存增量；单文件超过 8 MB 轮转成 `revisions.1.jsonl`。
- `kind`：`cell` / `style` / `merge` / `structural`（结构改动存摘要 + 影响范围，不存整表）。
- `epoch` 对齐保存序号，便于把「某次保存之后」的改动筛出来。
- 不进 xlsx：Excel 不认识自定义部件，我们的 `prune_orphans` 也会清掉它；放同目录还有个好处——
  共享盘上换个人打开也能看到历史（和现有 `.~sheetflow-history` 版本备份同一个目录）。

### 界面

审阅页 →「修订记录」：
- 列表：时间、人、位置（`Sheet1!B10`）、动作、前后值摘要；按人/类型/时间段过滤。
- 点一条：跳到该格并高亮；「撤回此条」用现有 undo 机制反做，但先校验当前值仍等于 `after`，
  不等就提示冲突（协作场景不硬盖别人的新值）。
- 撤回本身也记一条 `kind:"revert"`。

### 协作

宿主 append；新加入者连上后拉最后 200 条填充面板（不参与求值，纯展示）。

### 验收

- 连改 3 格 → 3 条记录，前后值正确；撤回第 2 条 → 值回退、新增 revert 记录。
- 两人协作：对方的改动进日志，who 正确（用现有两台进程的会话测法）。
- 152 MB / 43 万格文件改一格：日志追加 < 1 KB，保存耗时不变（日志异步写）。

## 3. 带公式的剪贴板

### 方案：XML Spreadsheet 主路 + HTML 兜底

实测样本（1,318 B，UTF-8）：

```xml
<Worksheet ss:Name="Sheet1"><Table ss:ExpandedColumnCount="2" ss:ExpandedRowCount="2">
  <Row>
    <Cell ss:StyleID="s63"><Data ss:Type="Number">1</Data></Cell>
    <Cell ss:StyleID="s63" ss:Formula="=RC[-1]+R[1]C[-1]"><Data ss:Type="Number">3</Data></Cell>
  </Row>
  <Row>
    <Cell ss:StyleID="s63"><Data ss:Type="Number">2</Data></Cell>
    <Cell ss:StyleID="s63" ss:Formula="=R[-1]C[-1]*10"><Data ss:Type="Number">10</Data></Cell>
  </Row>
</Table></Worksheet>
```

- `<Style>` 里有 `FontName/Size/Color/Bold/Italic`、`Interior`、`Borders`、`NumberFormat` ——
  比现在 HTML 路径拿到的还全（HTML 没有字体名）。
- `<Cell ss:Index>` 跳过空格、`ss:MergeAcross/MergeDown` 是合并、`ss:Formula` 是 R1C1、
  `<Data ss:Type>` 是 Number/String/Boolean/DateTime/Error。

### 实现步骤

1. `office_clipboard.rs`：加 `read_format_raw`（二进制不能按 NUL 截断）+ 注册 `"XML Spreadsheet"`；
   `read_table()` 先试 XML SS，失败再走 HTML。
2. 新模块 `crates/core/src/clipboard_xmlss.rs`（可单测，不碰 Win32）：
   - 解析 `<Styles>` → `CellStyle` 子集（字体名、字号、粗斜、字色、底色、数字格式、边框）；
   - 解析 `<Table>/<Row>/<Cell>` → `OfficeCell { text, kind, formula, style, colspan, rowspan }`；
   - **R1C1→A1**：`R[±n]C[±n]`（相对）、`RnCn`（绝对）、`R`/`C`（整行/整列）、`RC` 当前格；
     以该格坐标为基准，产出 `$A$1`/`A1` 形式交给现有引擎。约 80 行，配 10 条用例。
3. `OfficeCell` 增加 `formula: Option<String>` 与 `kind`；`paste_office_block` 里
   `Cell { value: 缓存值, formula }`，粘完统一 `recalc_all`（一次事务，撤销仍是一步）。
4. 保留 HTML 路径不动（无公式时的行为完全不变），XML SS 解析失败自动回落。

### 验收

- 从 Excel 复制 `A1=1, A2=2, B1==A1+A2, B2==A1*10` → 粘到我们这：B1/B2 是公式（不是 3/10），
  改 A1 → B1 跟着变；存盘后用 Excel 打开 B1 仍是 `=A1+A2`。
- 复制带合并/底色/粗体/数字格式的区域：结果与 HTML 路径一致或更好（字体名现在也能带上）。
- 复制纯文本/网页表格：仍走旧路径，行为不变（回归）。

## 建议顺序

| 顺序 | 项 | 改动面 | 主要风险 |
|---|---|---|---|
| 1 | 跨工作簿引用 | core: 新模块 + 词法/语法/求值 + 存盘刷新；app: 链接对话框 | 中：`[N]` 与文件名的双向映射、Excel 打开校验 |
| 2 | 剪贴板公式 | core: 新解析模块；app: 剪贴板与粘贴路径 | 低：XML SS 结构简单，R1C1 转换要覆盖全 |
| 3 | 修订跟踪 | core: 日志模块；app: txn_end/远端处 + 面板 | 低：纯应用层，不动文件格式 |

先做外部链接：它现在是「打开文件重算就显示 #VALUE!」的功能缺口，也是三者里唯一牵扯文件格式、
必须拿 Excel 验的一项；剪贴板是纯输入增强；修订跟踪最后做，因为它的日志格式最好在协作/撤销
都稳定之后再定型。
