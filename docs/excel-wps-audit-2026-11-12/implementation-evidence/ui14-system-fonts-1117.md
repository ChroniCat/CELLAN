# UI-14.1 收口：系统字体、真实粗体字面与真实度量（2026-11-17）

原 `not_done` 两条，逐条给证据。

## 1. 「真实字重未加载」→ 已加载

`crates/app/src/system_fonts.rs` 已有 `load_bold()`：

- 候选：`msyhbd.ttc`（微软雅黑 Bold）、`msyhbd.ttf`、`simhei.ttf`（黑体本身即粗）、
  Arial/Calibri/Times/Cambria/Consolas/Segoe UI 的 `*bd.ttf` 等；
- TTC 用 `ttf-parser` 按 family 名选 face，不按代码页猜；
- 有真实 bold 就注册 `sysbold:<family>` 族，`bold_font()` 优先用它；
- 系统没有粗体字面（宋体/新宋体只有常规）时，**不再合成加粗**，直接用内嵌
  `*-Bold` 真 700 字面（SIL OFL），PDF 走 `/F2`。

测试（本机实跑）：

```
cargo test -q -p app --bin app -- system_fonts --test-threads=1
running 4 tests
test result: ok. 4 passed; 0 failed
```

其中 `loads_real_bold_face_when_present` 断言本机 `load_bold("微软雅黑").is_some()`
且 `load_bold("宋体").is_none()`——正好覆盖「有粗体用系统、没有回落内嵌」两条路。

## 2. 「真实字体度量未用于换行/列宽」→ 已改用 Excel 口径度量

- `crates/core/src/display.rs::text_width()`：Carlito（度量兼容 Calibri）advance，
  CJK 按 1em；列宽/AutoFit 全部走它（`crates/app/src/main.rs` 的 SC_AUTOFIT 分支）。
- 列宽单位：内部 px = 字符数 × 8 + 5（Excel 16.0 实测，见 `ui14-autofit-1117.md`）。
- 折行模型：`floor((列宽pt − 4 − 缩进) × 0.88 / (0.975 × 字号))`，6 组 Excel 实测逐组一致。

AutoFit 三方实测（同一文件 `work/probe/autofit_in.xlsx`）：

| 列 | Excel ColumnWidth | 本程序 |
|---|---:|---:|
| A 中文 宋体 11 | 41.5 | 41.5 |
| B 英文 Calibri 11 | 27.63 | 27.63 |
| C 16 位数字 General | 10.63 | 16.5（General 数字显示策略差异，见 UI-14.3） |

## 结论

两条 `not_done` 均已落地；系统字体/真实粗体/度量三条链路都有测试或三方实测数字，
不是「看着像」。UI-14.1 记已验收；长整数 General 显示差异属 UI-14.3，不挂在本项。
