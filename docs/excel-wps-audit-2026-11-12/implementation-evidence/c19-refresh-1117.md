# C19.1 逐系列引用：校验与缓存重算（2026-11-17）

## 实现

`chart_write::refresh_series_from_refs(wb, sheet_name, &mut chart)`：

- 每条系列的 `val_ref` / `cat_ref` 用 `sparkline::parse_ref` 解析（含 `'表名'!$A$2:$A$6` 与
  `表名!A2:A6` 两种写法）；
- 表名不存在、引用语法坏、区域越界取不到数 → `Err`，调用方**不得写出**；
- 合法时按引用重读：`vals`、`cats`（散点另填 `xv`，X/Y 任一非数值的行整行丢弃）；
- 分类与数值必须同一张表，跨表直接拒绝（Excel 也不允许系列分类与数值分属两表时静默混用）。

`app::commit_chart` 在保存/插入/替换之前调用它：

```rust
let mut ch = ch;
if let Err(e) = refresh_series_from_refs(&self.wb, &name, &mut ch) {
    self.status = format!("图表引用无效：{e}");
    return;
}
```

即：对话框里手改引用后，看到的图与写进文件的值都来自引用本身，不是旧缓存；
引用写错会停在界面上报错，不会落盘成一个打不开/数据对不上的图。

## 夹具

`crates/core/tests/fixtures/c19/refresh.xlsx`（新，已入库；`.gitignore` 增加
`!crates/core/tests/fixtures/c19/*.xlsx`，此前整个 fixtures 目录被 `*.xlsx` 排除，
新克隆会缺夹具）：

| 列 | A 类别 | B 一 | C 二 | D X | E 文本X |
|---|---|---|---|---|---|
| 值 | 甲/乙/丙/丁 | 10/20/30/40 | 1/2/3/4 | 1.5/2.5/10/50 | a/b/c/d |

## 用例（3 passed）

- `changing_the_value_ref_rereads_the_cache`：把 `val_ref` 从 B 改成 C，
  刷新后 `vals` 变 `[1,2,3,4]`，`cats` 仍是甲/乙/丙/丁。
- `bad_or_empty_refs_are_rejected`：`Z1:Z3` → 「取不到任何数字」；
  `不是引用` → 「无效」。
- `scatter_refresh_keeps_x_y_in_pairs`：`cat_ref=D` 时 `xv=[1.5,2.5,10,50]`；
  换成文本列 E → 整行丢弃 → 报「取不到任何数字」。

## 与 C19.1 台账的关系

C19.1 原缺口「逐系列引用缺校验/重算」到此关闭；剩余只有真实键鼠打开对话框，
与父项 C19 一起记为无交互桌面阻塞。
