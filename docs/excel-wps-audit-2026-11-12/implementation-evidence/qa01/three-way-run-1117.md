# QA-01 三方矩阵实跑（2026-11-17）

命令：

```
python scripts/qa01_matrix_runner.py --out work/probe/qa01/matrix-run-1117.json
```

产物：`matrix-run-1117.json`（同目录已归档）。

## 结果

| 项 | 值 |
|---|---|
| 夹具 | 12 个（features / shapes / smartart / media / chart-ext / table-ext / page-ext / metadata-ext / pivot-ext / pivot-multi / features-xlsm / features-xls） |
| 我们的 load→save 往返 | **12/12 ok** |
| Excel 侧逐类 diff | **0**（12 个夹具 × 各自类别，共 76 个类别位） |
| WPS 侧逐类 diff | **0** |
| known_loss | 空（本次夹具未触发 `.xls` 结构性对象口径） |

`products`: `Excel.Application` + `ket.Application`（真 COM，不是模拟）。

## 口径说明（不掩盖的边界）

- 本 run 覆盖的是**格式与结构类**保真（styles/cf/dv/table/names/freeze/hidden/shapes/chart/smartart/pictures/links/page/metadata/pivot/values）；
- **未覆盖**：大簿（106MB 级）、二次保存刷新后的第三方再读、真实 UI 编辑后再存；
- `.xls` 结构性对象（CF/DV/表格/名称/冻结/隐藏、填充/边框颜色）按既定口径不保留，本次 `features-xls` 只登记 `values,numfmt`；
- WPS 读不出 Excel SmartArt 节点属对方限制（源文件同样如此）。

## 结论

QA-01.2 的「runner 未建立」已不成立；runner 实跑 12/12 往返、Excel/WPS 双端 0 diff。
剩余真实项：大簿与二次保存场景、真实 UI 编辑路径、父项证据链人工复查。
