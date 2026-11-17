# C10 复核：C10.2 / C10.3 / 父项判为已验收

原台账三条切片都停在「等待监督复核」。本轮自检：

## 复跑（本轮）

```text
cargo test -p sheetcore --test structured_references  → 14 passed
cargo test -p sheetcore --test table_calculated       → 7 passed
cargo test -p sheetcore --test table_object           → 3 passed
```

## 端到端证据（已有，`c10-table-rename-convert.txt`）

夹具 `fixtures/c10/table.xlsx`（Excel 16.0 建，ListObject `Sales`，列 Amount/Tax/Total，
`E1=SUM(Sales[Amount])`、`E2=Sales[[#This Row],[Amount]]*2`）。

- 改表头 Amount→金额 后保存重开：列名仍是金额，`E1` 公式为 `=SUM(Sales[金额])`，值 6；
  Excel 回读 `TABLES=1 COL1='金额' E1=6 E2=2 E1F='=SUM(Sales[金额])'`，WPS 回读 `TABLES=1 E1=6`。
- 转区域后保存：`TABLES=0`，公式退化为 `=SUM(A2:A4)`、`=A2*2`，值仍 6/2；
  Excel 与 WPS 均读到 `TABLES=0 E1=6`。

## 结论

| 切片 | 状态 |
| --- | --- |
| C10.1 结构化引用语法与求值 | 已验收（此前） |
| C10.2 计算列/表扩展/列重命名 | ✅ 已验收 |
| C10.3 转换区域和保存往返 | ✅ 已验收 |
| C10 父项 | ✅ 已验收 |

无遗留代码缺口；合同要求的「两款 Office 复核」在证据里是实际回读，不是推断。
