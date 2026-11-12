# 恢复记录

- 分支 `main`（`origin/main`），HEAD `cf42c1d` + 本轮未提交改动（C04 全量）。
- 台账：5 已验收 / 3 待验收 / 53 待实施；切片 14 已验收 / 7 待验收 / 127 待实施。
  C02 待复核（14 pass、6 处变异已抓）；C04 待复核（本轮）。

## 本轮（C04 1900/1904，复核 02:02 + 02:22 后）

已核实真值：
- `array_shape` 14 passed / 0 failed，EXIT=0（在**恢复后的源码**上跑，diff 无 mutation 残留）。
- `save_failure_injection` 11 passed / 0 failed / 1 ignored，EXIT=0（round16_sfi）。
- `date_system` 10 passed / 2 ignored；`date_system_shared` 8 passed。
- round16：core 62 bins / 546 passed / 0 failed / 37 ignored；app 141；GUI 12 passed / 8 ignored。

本轮修的两个复核问题：
1. `clipboard_xmlss::iso_to_serial` 原来丢掉 `T` 后的时间 → 中午变午夜。现按 `hh:mm[:ss[.ms]]`
   折成一天的小数，1904/1900 两侧都有"带时间"断言，普通数字仍不当日期。
2. `charts::resolve_series_in` 的值轴原来走格式化文本再 `parse::<f64>()`，
   `1,234.50` / `12.50%` / `$5.00` / 日期格式全部 parse 失败 → 系列点整段丢失。
   新增 `read_ref_numbers` 取原始数值；分类/名称仍用显示文本。回归断言 4 格带
   千分位/百分比/货币/日期格式时 vals 原值不变、点数 4。

另：实机验收发现写包缺陷——字体无条件 `set_size(0)`/`set_underline("none")`/
`set_strikethrough(false)` 产生 `<sz val="0"/>` 等非法值，**Excel 16.0 拒绝打开**。
已改为只写设置过的属性，并加 styles.xml 断言。

实机结果文件（可核对）：`implementation-evidence/c04-probe/probe-result.json` +
两份夹具 + 两份 Excel 另存件；Excel 16.0 / WPS 12.0 的 Date1904、A1 显示、另存不漂移全部记录。

## 下一步
- 提交本轮（C04 修复 + 证据 + 台账）。
- 继续任务队列：按 task-index 选下一个可执行切片（UI-01 真实 IME/键盘仍阻塞在硬件取证）。
- C04 未执行项：真实剪贴板 E2E（Excel 复制 → 应用粘贴）、1904 的 .xls 与两款软件互通。

## 风险
- Excel COM 打开文件必须用**反斜杠**原生路径；用 `F:/...` 会报 0x800A03EC，
  容易误判成文件损坏。
- 刚写出的文件立刻被 Excel 打开偶尔失败，probe 脚本已带 4 次重试。
