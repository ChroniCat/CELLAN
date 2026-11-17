# C19.3 雷达样式 / chartex 保真 —— 实现与真机验证（2026-11-17）

## 1. 雷达样式（standard / marker / filled）

- `Chart.radar_style` 从 `c:radarStyle` 解析，写出时原样落回；空/非法值退回 `marker`
  （Excel 默认的「带数据标记」）。
- 绘图：`filled` 时先铺一个 50% 白混色的闭合面（Excel 用 50% 透明，这里以混色近似），
  不再画数据点标记；`standard`/`marker` 维持线 + 点。
- UI：图表对话框在雷达图下多一个「雷达样式」下拉（标准 / 带数据标记 / 填充）。
- 用例 `c19_radar` 3 条：写出值逐字、写入-读回、填充面存在且 marker 无面
  （按混色 RGB (161,184,225) + 顶点数判定，避免与散点/折线的 polys 混淆）。

Excel 真机（`work/probe/radarprobe.ps1`，文件由 `insert_chart` 写进**已有 drawing** 的工作表）：

```text
charts=2
ChartType=73      ← 原有散点图
ChartType=82      ← 我们插入的填充雷达（xlRadarFilled）
resaved
Excel 重存后 chart3.xml 仍为 <c:radarStyle val="filled"/>
```

### 顺带修的真 bug：往已有 drawing 追加图表

工作表已经挂着 `<drawing r:id>` 时，原实现另建 `drawingN.xml` + `chartN.xml`，
但这些部件没有任何工作表引用——**孤儿部件**：Excel 打开看不到新图，我们自己的
`load_charts` 也读不到（只有原来的图）。

现在改为：解析工作表 rels 找到现有 drawing，把新图表作为**新锚点**追加进去
（drawing rels 加 `rIdN`、drawing 里加 `oneCellAnchor`、`cNvPr id` 取现有最大值+1、
新 chart 部件登记 Content_Types）。追加锚点的 `<c:chart>` 必须**就地声明**
`xmlns:c` / `xmlns:r`——现有 drawing 的根只声明了 `xdr`/`a`，不声明就是 unbound prefix，
Excel 直接拒开（这一条是本轮第二个「Excel 打不开」的真原因）。

## 2. chartex（瀑布/直方等高级图种）

夹具 `c19/advanced.xlsx` 三张表：雷达（81）、瀑布（chartEx1）、直方（chartEx2）。
`scripts/c19_types_probe.ps1` 用 Excel 打开**我们自己另存的文件**
（`save_plain` 全量重写，不是原样复制）：

```text
雷达: charts=1 types=[81]
瀑布: charts=1 types=[119]
直方: charts=1 types=[118]
```

与夹具逐项相同，没有降级；`chartEx1.xml` / `chartEx2.xml` 与夹具**逐字节相同**
（985 / 948 字节）。`replace_chart` 对 `Unknown` 图种直接报错「只能保留原图」，
防止 2D 覆盖；打印时显示「（保留原图，不能在此编辑）」占位。

chartex 本体仍**不可编辑**（Excel 的高级图表格式差异太大，本版本只做保留与占位），
这是已记录的边界，不是本轮新增缺陷。

## 3. 回归

- core 全量 `CORE_EXIT=0`（111 个测试二进制，含新增 c19_radar / c19_secondary）
- app 全量 `APP_EXIT=0`（375 passed / 1 ignored，单线程）

## 4. 仍缺

- 真实键鼠打开图表对话框切雷达样式（无交互桌面，父项 C19 级阻塞）。
- chartex 的编辑能力（本版本明确不做，只保留原图）。
