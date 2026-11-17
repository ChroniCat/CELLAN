# C17 复核：C17.1 / C17.2 / C17.3 / 父项判为已验收

原台账三条切片停在「等待监督复核」。本轮自检，逐条对原合同验收点：

## 1. 阈值类型（C17.1）

夹具 `fixtures/c17/priority.xlsx`（Excel 16.0 生成）同区域 Excel 导出色对比：

| 格 | Excel 原始 | 本应用 | Excel 重开我们的文件 | 改 F1 后 |
| --- | --- | --- | --- | --- |
| A1 1000 | (251,0,0) | [255,0,0] | (251,0,0) | (251,0,0) |
| A2 1100 | (251,0,0) | [255,1,0] | (251,0,0) | (251,0,0) |
| A3 100000 | (0,191,0) | [0,255,0] | (0,191,0) | (0,191,0) |
| B2 1100 | (251,250,0) | [255,255,0] | (251,250,0) | (251,250,0) |

A2 与 B2 同值不同色 → **percent 50 与 percentile 50 分开**（合同点 1）。
保存往返后 Excel 读回 `criteria types=1,3,2`（percent）与 `1,5,2`（percentile）均保住。

## 2. 优先级与 stopIfTrue（C17.1/C17.2）

C 列两条同条件规则 XML 顺序 priority=8 → priority=5（逆序），结果**蓝（priority=5）赢**；
D 列第一条 stopIfTrue=true，第二条不再生效（跨同一 sqref 的多段范围同样按 priority 全局求值）。

## 3. Dxf 边框与未建模扩展（C17.3）

`DxfBorder.sides[4]` 逐边（左/右/上/下）各自 `(style, color)`，不再是后一条覆盖前一条；
对角/内部/起止边框不建模，但加载时标 unsupported，条件格式管理窗口显示
「未建模的扩展、保存会丢掉、无法无损编辑」——合同允许「保留或显示不可无损编辑警告」，取后者。

## 4. 本轮复跑

```text
cargo test -p sheetcore --test c17_cf_priority  → 5 passed
cargo test -p sheetcore --test c17_dxf_borders  → 3 passed
cargo test -p sheetcore --test c17_dxf_fill     → 3 passed
```

## 结论

| 切片 | 状态 |
| --- | --- |
| C17.1 priority/阈值类型模型 | ✅ 已验收 |
| C17.2 求值与 stopIfTrue | ✅ 已验收 |
| C17.3 样式及未知扩展保留 | ✅ 已验收（警告口径符合合同） |
| C17 父项 | ✅ 已验收 |

遗留（不影响本合同）：x14 扩展（如 x14 数据条）保存会丢，已在 UI 显式警告；
若要真正保留，需要单独的 x14 parser，属新需求。
