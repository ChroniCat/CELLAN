# C15 工作表保护 —— 自检复核（2026-11-17）

取消 Codex 监督后，按约束「等待复核类项由助手自行按原合同机检验收、必须有可复现证据」
对 C15 三个切片做复核。

## 1. 事务与模型（C15.1 locked/hidden）

```bash
cargo test -q -p sheetcore --offline --test c15_protection -- --test-threads=1   # 3 passed
cargo test -q -p sheetcore --offline --lib protection:: -- --test-threads=1      # 5 passed
```

覆盖：`sheetProtection` 属性读写（sheet/objects/scenarios/formatCells/formatColumns/
formatRows/insertColumns/insertRows/insertHyperlinks/deleteColumns/deleteRows/sort/
autoFilter/pivotTables/selectLockedCells/selectUnlockedCells）、`locked`/`hidden`
单元格样式位、公式隐藏（选中时公式栏为空，Excel 生成的夹具
`c15-formula-hidden-a1.png` / `c15-formula-visible-b1.png` 已记录）。

## 2. 权限矩阵与选择门禁（C15.2）

`c15-selection-gate.txt` 记录四个入口（鼠标按下、拖拽扩选、方向键、名称框定位，
以及后补的 Enter/Tab `advance_active`）都走同一套 `can_select` 判定；
`c15-live.xlsx` 上 Excel COM 复核过「锁定格拒绝写、未锁定格可写」。

## 3. 三方往返（C15.3）

本轮重新对拍：Excel 生成的 `c15/protected.xlsx` → 我方 `save_plain` 全量重写 →
Excel 打开两者，结果逐项一致（`scripts/c15_recheck.ps1`）：

```text
fixture:                          ours:
protected=True                    protected=True
A1_WRITE=denied                   A1_WRITE=denied
B1_WRITE=ok                       B1_WRITE=ok
B1_FMT=ok                         B1_FMT=ok
B1_FILL=ok                        B1_FILL=ok
B1_VALUE=INPUT2                   B1_VALUE=INPUT2
final_protected=True              final_protected=True
```

即：保护状态、锁定/未锁定区域的写权限、未锁定区域允许格式化、密码保护本身都在
我们重写后保持，Excel 侧行为与原始夹具完全相同。

## 结论

- C15.1 / C15.2 / C15.3 → 已验收（复核依据：上述命令输出 + 本文件 + 既有 png/txt 证据）。
- 父项 C15 仍留「真实键鼠在窗口里逐项点选」一项：环境 `GetForegroundWindow()=0`，
  `Start-Process` 与 SendInput 均不可用，属阻塞项，不写成已验。
