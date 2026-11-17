# C20.1 中文笔画排序（2026-11-17）

台账原缺口：「笔画/部首排序不做：需要可分发授权的离线笔画数表，拼音序不能冒充笔画序」。

## 1. 做法

不引第三方笔画表，而是把**本机 Excel 的笔画排序结果**当作基准实测出来：

1. 从 Unihan（`kTotalStrokes`）取 CJK 基本区 + 扩展 A 的 27 584 个汉字；
2. 一次性交给 Excel 16.0，`Worksheet.Sort` + `SortMethod = xlStroke` 升序；
3. 读回结果顺序，写成 `assets/stroke_order.bin`（2 字节/字，小端码位，下标即名次，
   55 KiB）；生成脚本 `work/probe/stroke_sort.ps1`，说明见
   `assets/stroke_order.README.md`；
4. `collate::cmp_stroke` 用该表逐字比名次；表外字符（假名、字母、扩展 B 以后）
   整串退回拼音序，保证仍是全序。

这样得到的是 **Excel 的真实笔画序**，不是按笔画数近似——同画数内部的次序也照抄
（例：4 画 `王 天 木 中 水 火`，5 画 `石 目 田 白`，都不是部首序或码位序）。

## 2. 真机对拍

- 单字抽样：Excel 对 28 字排出的顺序 `一乙二十人三大小山川王天木中水火石目田白耳虫竹自行衣羊米`
  与表内名次严格递增一致（`crates/core/src/collate.rs` 的
  `stroke_rank_follows_excel_xlstroke_sample`）；
- **多字串**：300 个随机 1–3 字串（含扩展 A 生僻字），同一份输入分别由
  Excel xlStroke 与 `collate::cmp_stroke` 排序，
  **300/300 完全一致**（`work/probe/stroke_parity_xl.ps1` +
  `crates/core/examples/stroke_order.rs`，输入 `stroke_parity_in.xlsx`）。

## 3. 用例

```bash
cargo test -q -p sheetcore --offline --test c20_stroke -- --test-threads=1
# stroke_sort_matches_excel_order        一 王 李 张（Excel 名次 0/163/1758/2530）
# stroke_and_pinyin_are_different_orders 拼音是 李 王 一 张
# stroke_sort_is_total_and_stable_for_unknown_chars
# 3 passed
```

## 4. 界面

排序对话框「排序依据」里原来的灰项「笔画（本版本不支持）」换成可选的
**「中文笔画」**，hover 说明写清「按 Excel 笔画排序（xlStroke）的实测顺序；
表外字符退回拼音序」。`sort_mode = 3` 映射 `SortMode::Stroke`（`main.rs`）。

## 5. 仍缺

- 固定宽度分列与排序对话框的**真实键鼠**操作（无交互桌面，C20.1 级阻塞）。
