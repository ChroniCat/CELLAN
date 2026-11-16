# 图表锚点：resave 暴露的真实缺陷与修复

## 现象

`qa01_matrix_runner.py --resave` 在 `chart-ext` 上：

```
[chart-ext] resave: excel 0 / wps 1
    wps ! sheets[0].shapes[0].h: 191 != 202
```

## 定位

同一文件在 WPS 下重复测量是稳定的（`orig` 两次都是 191，`ours` 两次都是 202），
说明不是抖动。解包对比：

| 文件 | drawing1.xml 里图表锚点 |
|---|---|
| 原文件 | 没有 `<xdr:ext cx cy>`（保留 Excel 原始的 oneCell/精确尺寸写法） |
| 我们的副本 | `<xdr:ext cx="5400000" cy="2700000"/>` + 两单元格锚点 |

根因：`chart_write::write_anchor()` 每次保存都把锚点重写成「两单元格 + 按单元格推算的 ext」，
即使图表没动过，也会把原来的精确尺寸（191px）换成默认尺寸（202px）。
Excel 侧因为会归一化显示，diff 为 0；WPS 直接暴露了尺寸变化。

## 修复

`write_anchor()` 在重写前先比较「锚点单元格序列」：

```rust
if !edited.is_empty() && anchor_cells(&edited) == anchor_cells(&xml) {
    return Ok(());   // 单元格没变 → 原样保留 drawing，ext 不被改写
}
```

新增 `anchor_cells()` 提取 `<col>`/`<row>` 数值序列做对比。

## 验证

1. `python scripts/qa01_matrix_runner.py --only chart-ext` → `excel 0 / wps 0`；
   WPS 读到的形状高度回到 **191**（与原文件一致）。
2. `cargo test -p sheetcore --lib chart` → **7 passed**。
3. `--resave` 四夹具重跑在 `work/goal-logs/qa01-resave-b.log`（结果另档）。

## 备注

这条属于「resave 场景」才能暴露的缺陷：普通往返里两单元格锚点恰好被 Excel 归一化成同一显示尺寸，
只有第三方（WPS）按原始尺寸报数时才看得见。说明 `--resave` 这一层是必要的，不是重复劳动。
