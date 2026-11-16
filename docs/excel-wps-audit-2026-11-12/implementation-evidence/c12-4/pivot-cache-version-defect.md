# 更正：不存在"Excel 丢弃透视表"缺陷（原判断是探针假象）

## 结论先行

`work/probe/c12/pivot-real.xlsx`、`c12-4-multi/multi-A.xlsx`、`c12-4-2cols/multi-D.xlsx`
（全部是**修复前**生成、都没有 `refreshedVersion`）在稳定探针下全部正常：

```
pivot-real.xlsx  sheet=Data pivots=1 rows=1 cols=1 data=1 range=A9:D13
multi-A.xlsx     sheet=Data pivots=1 rows=1 cols=1 data=1 range=A9:C16
multi-D.xlsx     sheet=Data pivots=1 rows=1 cols=1 data=1 range=A9:J14
```

因此前一轮"缺 createdVersion/refreshedVersion/minRefreshableVersion → Excel 静默丢弃"的
因果判断**不成立**，`hybrid2/hybrid3` 的成功是同一批抖动读数的巧合。

## 真正的原因：Excel COM 探针写法不稳

错误写法（会返回空、`MethodNotFound`、或凭空 0）：

```powershell
$ws.PivotTables()          # 返回 null
@($ws.PivotTables)         # 枚举出的是 PSMethod，不是集合
$ws.PivotTables.Item(1)    # PSMethod 没有 Item
"pivots=" + $ws.PivotTables.Count   # 与其它写入混在一行时被吞
```

稳定写法（同一批文件复现 100% 一致）：

```powershell
$pts = $ws.PivotTables.Count      # 注意：不要加 ()
$pt  = $ws.PivotTables(1)         # 参数化属性直取第 1 个
```

外加：**每个文件单独起一个 Excel 实例**、`Open($path, 0, $true)` 只读、打开后等 1.5s、
`Quit()` 后 `ReleaseComObject`。脚本见 `scripts/pivot_excel_probe.ps1`。

## 保留下来的改动

`pivot_parts.rs` / `pivot_parts2.rs` 仍补写 `createdVersion="8" refreshedVersion="8"
minRefreshableVersion="3"`——这是 Excel 自己保存时必写的属性，补上属于加固，
`pivot_real` 里的断言保留为结构回归。**但它不是任何缺陷的修复**，不能记成 fix。

## 教训

同一进程里连续 Open 多个含透视表的文件时，读数会互相污染；把"读不到"当成"文件坏了"
之前，必须换干净实例复现一次。本轮因此产生了两次误判（本文件 + `wps-roundtrip.md`）。
