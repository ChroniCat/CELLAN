# C19.2 次坐标轴刻度 / 散点 x·y 建模 —— 实现与真机验证（2026-11-17）

## 1. 次坐标轴刻度

`print::draw_chart` 原来只画主轴刻度，次轴只有一条竖线。现在次轴按与主轴相同的
「整齐数」规则另起一套：

```rust
let max_sub = nice_max(max_of(true));          // 已有
let want2 = if ch.y_fmt.contains('%') { 10.0 } else { 5.0 };
let step2 = nice_step(max_sub / want2);
let n2 = ((max_sub / step2).ceil() as usize).clamp(1, 12);
// 标签画在 px0 + pw + 3.0（右侧）
```

用例 `secondary_axis_gets_its_own_nice_ticks`：主系列 80/100（主轴 0/20/…/100），
次系列 2.0/3.7 → `nice_max` = 5、步长 1；断言页面文本里出现 `4`、`5`
（主轴刻度集合里没有这两个值，所以只能来自次轴）。

## 2. 散点 x/y

原来 `chart_from_selection` 对散点也把第一列当**分类名**，`Series.xv` 永远是空，
写出时 `xVal` 退回「序号 1..n」——X 非等距的散点会被画成等距。

现在：

- 选中 `ChartKind::Scatter` 时，第一列按数值解析成 X；X 或 Y 非数值的行**整行丢弃**，
  保证 `xv` 与 `vals` 一一对应（历史 bug：空 Y 跳过会让 x/y 错位）。
- `scatter_body` 写 `xVal` 时优先用 `xv`（等长时），否则退回分类列解析/序号。
- 绘图侧 `print.rs` 本来就会用 `xv` 画点（`scatter_points_follow_x_scale_not_index` 已覆盖）。

用例：

- `scatter_selection_models_x_and_y_pairs`：X 列 1/2/10/50/100 → `xv` 完全一致，
  XML 里 `<c:pt idx="2"><c:v>10</c:v>` 而不是 `3`。
- `scatter_pairs_survive_write_and_reload`：插入后自家读回 `xv`/`vals` 不变。

## 3. Excel 真机回读

`scripts/c19_scatter_probe.ps1` 打开我们生成的 `c19-scatter.xlsx`：

```text
type=73
series=Y n=5 X=[1,2,10,50,100] Y=[2,4.5,5,9,12]
```

`ChartType=73` 即 `xlXYScatter`；`XValues` 是真实的非等距 X，不是 1..5 的序号。
这同时证明上一轮修的 `xmlns:r` / `dLbls` 顺序问题没有回归——文件能被 Excel 正常打开。

## 4. 用例清单

```bash
cargo test -q -p sheetcore --offline --test c19_secondary -- --test-threads=1
# secondary_axis_gets_its_own_nice_ticks
# scatter_selection_models_x_and_y_pairs
# scatter_pairs_survive_write_and_reload
# 3 passed
```

回归面：`c16_print` 10、`c16_objects` 4、`c19_axes` 7、`c19_labels` 3 全绿。

## 5. 仍缺

- 真实键鼠打开对话框选图型/改引用（无交互桌面，父项级阻塞）。
- 次轴的独立数字格式与轴标题（Excel 允许次轴单独设格式；当前共用主轴 `y_fmt`），
  未建模，不写成已支持。
