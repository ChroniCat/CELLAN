# C07 复核：C07.1 / C07.2 判为已验收

监督取消后按原合同自检，两类证据：

## 1. 内部语义（C07.1 OFFSET / 跨表 / 负高度）

`crates/core/tests/c07_dv_{refs,offset,dynamic,style}.rs` 全绿：
**1 + 2 + 2 + 5 = 10 passed**（`--test-threads=1`）。

Excel 16.0 COM 真值表与实现对照见 `c07-dv-offset.txt`（省略 height 继承 3 行、
跨表 `COUNTA($A:$A)` 按主表算、负高度反向展开 `$A$2:$A$3`、越界 `#REF!` 视为配置无效）。

## 2. 双端保真（C07.2 提示等级 / 错误处理 / 输入提示）

`fixtures/c07/alerts.xlsx`（Excel 建：B2 stop+输入提示、C2 warning、D2 information、E2 showError=0）
经我们引擎往返后：

```text
dataValidation 四条属性串逐字节相同：SAME SAME SAME SAME
```

Excel 与 WPS 对原文件/我们文件读出的 four-cell 结果完全一致（type/showErr/showIn/errmsg/f1）：

```text
Excel orig/ours B2 type=1 showErr=True showIn=True  errmsg=只能填 1-10，确定后回到编辑框
             C2 type=1 showErr=True showIn=False errmsg=可以继续，但请确认这个值
             D2 type=1 showErr=True showIn=True  errmsg=这个值不在建议范围内
             E2 type=1 showErr=False showIn=False errmsg=未启用错误警告
WPS   同上四条，逐字一致
```

注意：探针里 `ErrorStyle` / `PromptTitle` 读出为空是 PowerShell late-binding 假象
（同 `.Item(1)`、`QuotePrefix` 那类），所以等级判定以 XML 属性串全等 + 双端读数一致为准。

## 结论

| 切片 | 状态 |
| --- | --- |
| C07.1 命名/区域/公式阈值求值 | ✅ 已验收 |
| C07.2 停止警告信息及输入提示 | ✅ 已验收 |
| C07.3 与键盘下拉和保存集成 | 保存侧已验（上面往返）；**键盘下拉的真实按键未测**（无交互桌面），留待验收 |
| C07 父项 | 待验收（C07.3 未闭合） |
