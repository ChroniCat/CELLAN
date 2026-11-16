# QA-01 大簿场景（合成 200k 行）

## 夹具

`work/probe/qa01/large-synth.xlsx` —— `scripts/qa01_make_large.py` 生成：

| 项 | 值 |
|---|---|
| 规模 | 200,000 行 × 6 列，**7.8MB** |
| 列 | 编号 / 金额 / 数量 / 名称 / 单价 / 小计（`=B*C` 真公式，20 万条） |
| 特性 | 条件格式（B>5000 红底）、数据验证（C 0–100 整数）、自动筛选、冻结首行、横向打印 + 重复标题行 |

**表格为什么换成自动筛选**：openpyxl 写出的 `<table>` 会被本机 Excel 16.0 拒绝打开
（`不能取得类 Workbooks 的 Open 属性`）；用 500 行样本按特性 bisect，只有加 table 才失败，与行数无关。
表格保真由 `table-ext` 夹具覆盖（Excel 自己创建）。

## 结果

```
[large-synth] excel: 意外 0 处 / 已登记损失 0 处
[large-synth] wps:   意外 0 处 / 已登记损失 0 处
entries=1 with_diffs=0
EXIT=0
```

- 我们的 load→save 往返：**ok**（输出 7.8MB）
- Excel 端逐类 diff：**0**（values / cf / dv / freeze / page）
- WPS 端逐类 diff：**0**

## 边界

- 这是**合成大簿**，不是原始 106MB / 21 表压力样本（该文件在本机已不存在）；
- 大簿的性能指标（打开/保存耗时）属 QA-07，不以此 run 代替；
- 二次保存组合正在单独跑（`qa01-large-resave.log`）。
