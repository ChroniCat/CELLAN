# QA-06.1 SUBTOTAL/AGGREGATE 隐藏行口径（Excel 实算）

日期：2026-11-17　Excel 16.0 Build 20326
夹具：`scripts/qa06_subtotal_probe.ps1` → A1:A10 = 1..10，**手动隐藏第 3、5 行**，S2!A1=100。

| 单元格 | 公式 | Excel | 含义 |
|---|---|---|---|
| C1 | `=SUBTOTAL(9,A1:A10)` | **55** | 1–11 系列：**包含**手动隐藏行 |
| C2 | `=SUBTOTAL(109,A1:A10)` | **47** | 101–111 系列：**排除**手动隐藏行 |
| C3 | `=SUBTOTAL(3,A1:A10)` | **10** | COUNTA 同样包含隐藏行 |
| C4 | `=AGGREGATE(9,5,A1:A10)` | **47** | option 5 = 忽略隐藏行 |
| C5 | `=SUM(A:A)` | **55** | 整列引用不受隐藏影响 |
| C6 | `=S2!A1+1` | **101** | 跨表引用 |
| C7 | `=SUBTOTAL(1,A1:A10)` | **5.5** | 平均值同样包含隐藏行 |

## 本引擎现状（读码核对）

- `SUBTOTAL` 只做 `f % 100` 取基础函数再直接 `call(...)`，**完全不看隐藏行**：
  C1/C3/C7 会碰巧正确，C2（109 系列）会算成 55，错。
- `AGGREGATE` 同样没有处理 option 5/6（忽略隐藏行），C4 会算成 55，错。
- 整列与跨表引用（C5/C6）已有实现，这两条不是缺口。

## 改造落点

`Arg::Range` 只带 `values/rows/cols`，没有行区间信息，所以隐藏行过滤必须在**求值器构造实参时**做：
在 `mod.rs` 的 `Node::Func` 求值分支里，对 `SUBTOTAL`（首参 ≥100）与 `AGGREGATE`（第二参 5/6）
按 `sheet.hidden_rows`/`hidden_rows_user` 过滤掉对应行再打包 `Arg::Range`。
过滤器要同时认 `ht=0` 这种「只给行高不给 hidden 标记」的写法（与状态栏统计口径一致）。

## 复跑

```powershell
powershell -File scripts/qa06_subtotal_probe.ps1 F:/finance-native-sheets-ready/work/qa06h
```
