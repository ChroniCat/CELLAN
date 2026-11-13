# 恢复记录

## 2026-11-14 检查点：C16 打印四切片已实现

- 分支 `main`，HEAD `f31ce19` 已 push。
- 台账：已验收 24 / 61；待验收 5（UI-01、C07、C15、C17、C16）；实施中 7；待实施 25。
- C16（打印/PDF）四个切片全部实现，证据 `implementation-evidence/c16-print-layout.txt`、
  `c16-objects.txt`：
  - 新增 `crates/core/src/display.rs`：General 对齐、数字格式、[Red]、合并、折行、
    垂直对齐、缩进、条件格式叠加，屏幕/预览/PDF 共用；
  - `print::page_ops` 产 rects/lines/texts/polys/images，PDF 只多字形编码，
    预览直接画同一份 ops；分页正文从标题之后开始，标题每页重复且不与正文/标题列重叠；
  - 页眉页脚 `&L/&C/&R`、`&P`、`&N`；单段数字格式负号按 Excel 实测修正（`--987.65`）；
  - 图片（JPEG DCTDecode / PNG 解 IDAT+过滤器后 FlateDecode）、图表（柱/条/折线/饼 +
    标题/轴/图例）、文本框（EMU 换算 + 自动换行）；
  - 修复真实 bug：图表锚点 `<xdr:from><xdr:col>` 解析失败导致锚点全变 (0,0,4,3)、
    图表压住数据；修复后夹具解析 C1:I15。
- 测试：sheetcore **766 passed**、app `--bin app` **263 passed**；c16_print 6 条、
  c16_objects 3 条。PDF 用 pypdfium2 逐页取文本 + 渲染 PNG 核对（表格/图表/图片/文本框无重叠）。
- Excel 对拍差异如实记录：Excel 在 `print_title_rows=1:2` 与合并标题重叠时，把合并标题
  挪到第 2 页、并跳过合并换行块；我们按"标题每页重复、正文不重不漏"输出，不复刻该缺陷。
- GUI 约束继续有效：真实键鼠/IME 未执行（RDP 不可用），C16 及此前四项均留待验收，
  不用 `SC_*` 注入或截图冒充。

## 下一步

1. **C18 样式模型与未知样式保留**（待实施）：先做 C18.1 主题色/tint 解析——读
   `xl/theme/theme1.xml` 的 `clrScheme`，`<color theme="n" tint="t"/>` 按 tint 公式解析成
   RGB；保存时保留原始 theme/tint 属性，不能降级成 rgb。再评估图案填充与复杂边框。
2. QA-06 公式语义矩阵（纯核心，可与 C18 并行排期）。
3. 需要真实输入/IME 的项（UI-01、QA-04、UI-11、UI-24…）等可用桌面会话，不伪造。
