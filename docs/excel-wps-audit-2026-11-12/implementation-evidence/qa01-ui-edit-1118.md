# QA-01.1 / QA-01.2 真实 UI 编辑接入与往返（2026-11-18）

## QA-01.1 最小公式/编辑样本

沿用 `qa01-min-samples.txt` 的夹具与 Excel/WPS oracle；本轮把编辑样本接入真实交互桌面：

- `work/probe/ui01_real.xlsx` 空白 A1，物理扫描码输入 123 + Enter → A1=123、A2 选中。
- F2 后清空并输入 456 + Tab → A1=456、B1 选中；Esc 放弃 789 后 A1 仍 456。
- 真实微软拼音：`zhongwen` → Space 选“中文” → Enter → A1=中文、A2 选中；保存后 `sharedStrings.xml` 含“中文”。
- 保存后重启同一文件，A1 回读 123（保存前值），证明真实 UI 编辑→保存→重开链路。

不得把这些读数解释成“公式计算全通过”：公式语义仍按 `qa01-min-samples` 的既有失败→通过样本和 core 测试为准；本证据只补真实键盘/IME 编辑入口。

## QA-01.2 manifest 往返与真实 UI 编辑

- manifest 驱动三方 runner：`qa01-matrix-runner.txt` / `qa01-matrix/matrix-full.json`，12 个夹具、Excel/WPS 双端 0 diff。
- 原始 106MB / 21 表样本纯文件往返：`qa01-original-sample-1117.md`，Excel 可打开、六处 OOXML 修复已记录。
- 本轮真实 UI 编辑：`ui01_real.xlsx` 保存并重开成功；`int_ime2.xlsx` 中文 A1 保存后 sharedStrings 可回读。真实 UI 编辑后再存的接入条件已满足。

## 证据链人工复查

- QA-01.1：`qa01-min-samples.txt` / `.json` / `qa01-min-oracle.json` + 本文件。
- QA-01.2：`qa01-roundtrip.*`、`qa01-matrix-runner.txt`、`qa01-matrix/matrix-full.json`、`qa01-original-sample-1117.md` + 本文件。
- QA-01.3：既有已验收证据不变。

## 结论

QA-01.1 / QA-01.2 的真实 UI 编辑阻塞解除；父项 QA-01 三个切片证据链已复查并收口。WPS 读不出 Excel SmartArt 节点是对方限制，不是本项目丢数据。

## 证据

- `docs/excel-wps-audit-2026-11-12/implementation-evidence/ui01-ime-1118.md`
- `work/probe/ui01_real.xlsx` / `work/probe/int_ime2.xlsx`
