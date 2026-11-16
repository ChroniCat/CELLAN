# QA-01 二次保存（resave）全量结果

命令：

```
python scripts/qa01_matrix_runner.py --resave --out work/probe/qa01/matrix-resave-all2-1117.json
```

链路：原文件 → 我们 load+save → **Excel 打开并原样重存** → 我们再 load+save → Excel/WPS 双端快照比对。

## 判定口径

- 基线是「**Excel 重存后的中间态**」（`diffs_vs_resaved`）——这一段才是我们该负责的；
- 「相对原文件」（`diffs`）只作信息字段，因为 Excel 自己重存会做归一化（如 chart 锚点 ext）。

## 结果

| 夹具 | vs Excel 重存（Excel / WPS） |
|---|---|
| features / shapes / smartart / media | 0 / 0 |
| chart-ext | **0 / 0** |
| table-ext / page-ext / metadata-ext | 0 / 0 |
| pivot-ext / pivot-multi | 0 / 0 |
| features-xlsm | 0 / 0 |
| features-xls | **跳过**（旧 BIFF：Excel 重存后目标文件消失，无法作为基线；结构性对象在 QA-01.3 既定口径外） |

`EXIT=0`，无失败项。

## 过程中修掉的两件事

1. **图表锚点被重写**：`write_anchor()` 每次保存都把 oneCellAnchor 的精确 `ext` 换成两单元格推算尺寸，
   WPS 读到高度 191→202。现在锚点单元格没变就**原样保留 drawing**。
2. **脚手架残留**：目标文件已存在时我们的保存走「就地补丁」，会沿用旧部件（残留 twoCellAnchor）；
   `our_roundtrip()` 改为每次先删目标文件。

## 仍未覆盖

- 大簿（合成 200k 行夹具已备，单独跑）；
- 原始 106MB 压力样本（本机已无该文件）；
- 真实 UI 编辑后再存（无交互桌面）。
