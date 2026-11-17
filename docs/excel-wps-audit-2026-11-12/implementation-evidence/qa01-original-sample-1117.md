# QA-01.2 证据：原始 106MB / 21 表压力样本真实往返（2026-11-17）

## 样本从哪来

台账此前写「原始 106MB / 21 表压力样本本机已不存在」。这次在本机回收站
`F:\$RECYCLE.BIN\S-1-5-21-...\$R66Z71V\lp2.xlsx` 找到并复制到
`work/probe/recovered/lp2.xlsx`：

- 107,122,772 字节（约 102.2 MiB）；
- 21 张表（测试导航 / 参数与字典 / S1…S13 / 基础数据 / 明细 / 汇总 / 历史 / 透视 / 图片）；
- XML 统计：201,945 行、4,186,774 个 `<c>`、2,068,997 个 `<f>`；
- 原文件 Excel 16.0 可打开（21 表，`S10_现代数组` UsedRange `$A$1:$P$45`）。

## 往返命令与结果

```
target/release/examples/qa01_roundtrip.exe \
  work/probe/recovered/lp2.xlsx work/probe/recovered/lp2_rt3.xlsx
-> rc=0, 60s, {"sheets":21,"date_system":"D1900"}
```

- 输出 104MB；21 张工作表的 XML **SHA-1 全部与原文件逐字节相同**（diff=[]）；
- 行/格/公式计数：201,945 / 4,186,774 / 2,068,997，与原文件完全一致；
- Excel 16.0 `Workbooks.Open` 成功：21 表；
- WPS 12.0 `KET.Application` 打开成功：21 表；
- `S10_现代数组` 抽查（Excel COM 读回，两边一致）：
  `E14=4 F13=2 O18=1 E15=7 F14=5`，
  `J13` 公式 `=FILTER(A6:A11,B6:B11="华东")`，值 1。

## 这次抓出并修掉的两个真 bug

### 1. 载入迷你图就把表标 dirty

`xlsx.rs` 载入 `x14:sparklineGroups` 后无条件 `sparklines_dirty = true`。
保存时 `sparkline::apply` 重写整段 extLst，丢掉了模型外的
`lineWeight`、`xr2:uid` 等属性。Excel 对 S7_样式矩阵 的 extLst 直接判文件损坏
（`Workbooks.Open` 报 `0x800A03EC`），WPS 仍能打开。

修正：载入只解析、不标 dirty；只有真正编辑迷你图时才置 dirty。
回归：`cargo test -p sheetcore --test sparklines`（5 passed），并新增
「载入后 `sparklines_dirty=false`」断言。

### 2. 全量重算把未编辑的表标成 dirty

`formula::recalc_all_force_impl` 通过 `write_back` 写回结果时会置
`sheet.edited=true`。对 S10_现代数组 这类动态数组表，保存时不再走
`restore_unedited_sheets`，整表被重写：

- 28 个溢出缓存格（E14/F13/O18…）被当成 spill 丢掉；
- 数组公式前缀 `_xlfn._xlws.` 被剥掉（`FILTER`/`SORT` 变成裸函数名）；
- Excel 再次拒收整包。

修正：重算前后快照/恢复 `Sheet.edited`，重算本身不算用户编辑；用户真正改过的表
仍然保持 dirty。回归：新增
`recalc_does_not_mark_sheet_edited`（lib 测试，1 passed）。

## 修复后的再次验证

- `qa01_roundtrip`（不重算）→ `lp2_rt3.xlsx`：21 表 XML 全同，Excel/WPS 都能开；
- `recalc_all` + `save` → `lp2_recalc2.xlsx`：行/格/公式计数仍
  201,945 / 4,186,774 / 2,068,997，Excel 打开 21 表成功。
  说明「打开 → 重算 → 保存」这条真实路径也不再破坏原样本。

## 仍缺

- 真实键盘/IME 在 GUI 里改一格再保存（本机无交互桌面，SendInput/SendKeys 被拒）；
  该项仍归 QA-01.1/QA-04.1 的阻塞。
- 父项证据链人工复查。
