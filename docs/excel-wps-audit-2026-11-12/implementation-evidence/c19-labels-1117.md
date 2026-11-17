# C19.1 数据标签逐项选项 —— 实现与真机验证（2026-11-17）

## 实现

- `charts::LabelOpts`：`show_cat / show_ser / show_pct / show_legend_key / leader_lines / position`，
  与既有 `show_labels`（显示值）一起构成 Excel「设置数据标签格式」的那组勾选。
- 解析：`charts.rs` 从 `c:dLbls` 读 `showCatName/showSerName/showPercent/showLegendKey/showLeaderLines/dLblPos`。
- 写出：`chart_write::dlbls_xml` 按 OOXML 顺序
  `showVal → showCatName → showSerName → showPercent → showLegendKey → showLeaderLines → dLblPos`。
- **同时修了两个会让 Excel 拒开文件的真 bug**（本次才暴露）：
  1. `c:dLbls` 原来写在 `c:cat/c:val` 之后；CT_BarSer/CT_LineSer 要求 dLbls 在 cat/val **之前**，
     且 `spPr` 必须紧跟 `tx`。已重排 bar/line/scatter 三种 ser。
  2. `insert_chart` 往工作表写 `<drawing r:id>` 时，工作表根常常没有 `xmlns:r` 声明 →
     未绑定前缀，Excel 打开直接 `COMException：不能取得 Workbooks 的 Open 属性`。
     现在插入前就地补 `xmlns:r`。
- UI：图表对话框加六个勾选（值/类别/系列/百分比/图例项/引导线）、标签位置下拉，
  以及「逐系列引用（高级）」折叠区（名称/分类/数值三个引用可编辑）。

## 真机验证（Excel 16.0 COM）

`scripts/c19_labels_probe.ps1` 打开我们生成的文件：

```text
charts=1
series=部门 HAS=1 ShowValue=True ShowCategoryName=True ShowSeriesName=True ShowLegendKey=True Position=2
series=金额 HAS=1 ShowValue=True ShowCategoryName=True ShowSeriesName=True ShowLegendKey=True Position=2
```

`Position=2` 即 `xlLabelPositionOutsideEnd`，对应我们写的 `outEnd`。

饼图（`PIE_KEPT`，ChartType=5）：

```text
Excel 重存后 chart1.xml 的 dLbls：
<c:dLblPos val="bestFit"/><c:showVal val="1"/><c:showCatName val="0"/>
<c:showSerName val="0"/><c:showPercent val="1"/><c:showLeaderLines val="1"/>
```

即 Excel 认可 `showPercent=1`、`showLeaderLines=1`、`dLblPos=bestFit` 并原样保留。

## 用例

```bash
cargo test -q -p sheetcore --offline -j 2 --test c19_labels -- --test-threads=1
# dlbls_xml_lists_every_option_in_excel_order
# label_options_survive_write_and_reload
# pie_percent_and_leader_lines_are_written
# 3 passed
```

## 仍缺（C19.1 不结项）

- 逐系列引用的**输入校验**（非法区域要拒绝）与编辑后缓存重算，本轮只提供可编辑入口与写出通路。
- 真实键鼠点开图表对话框（无交互桌面，阻塞项）。

## 回归与修正（同日）

第一版 `xmlns:r` 注入直接 `find('>')`，命中了 `<?xml ... ?>` 声明里的 `>`，把属性插进声明，
写出 `<?xml version="1.0" ... xmlns:r="..."?>` → umya 报 `UnclosedXmlDecl`，
连带 `save_failure_injection::injected_chart_failures_abort_the_save` 与
`app::chart_save_acknowledges_a_new_sheet_before_the_next_save` 失败。改为先跳过 `?>` 再找根标签的 `>`：

```text
decl_end = x.find("?>") + 2
root_end = decl_end + x[decl_end..].find('>')
```

修后两条用例都恢复通过（`save_failure_injection` 11 passed / 1 ignored，
app `chart_save_acknowledges...` 1 passed），Excel 也能正常打开并读回标签。
