# assets/stroke_order.bin

汉字笔画排序表（2 字节/字，小端 Unicode 码位，数组下标 = 排序名次）。

- 覆盖：CJK 基本区 U+4E00–U+9FFF + 扩展 A U+3400–U+4DBF 中 Unihan 有
  `kTotalStrokes` 的 27 584 字；
- 顺序来源：把上述字一次性交给本机 Excel 16.0，用
  `Worksheet.Sort` + `SortMethod = xlStroke(2)` 升序排出的实际结果
  （`work/probe/stroke_sort.ps1`），再按结果顺序写入本文件；
- 这是我们自己观察到的排序事实，不含任何 Microsoft 数据文件；
- 校验：`work/probe/stroke_parity_xl.ps1` 对 300 个 1–3 字随机串做同样的
  Excel 排序，与本仓 `collate::cmp_stroke` 的结果 **300/300 完全一致**
  （见 `docs/excel-wps-audit-2026-11-12/implementation-evidence/c20-stroke-1117.md`）。

重新生成（需要本机 Excel）：

```bash
python - <<'PY'   # 从 Unihan 取字集，写 stroke_input.xlsx
...
PY
powershell -File work/probe/stroke_sort.ps1        # Excel xlStroke 排序 → stroke_sorted.xlsx
python - <<'PY'   # 读回顺序 → assets/stroke_order.bin
...
PY
cargo test -p sheetcore --offline --test c20_stroke -- --test-threads=1
```
