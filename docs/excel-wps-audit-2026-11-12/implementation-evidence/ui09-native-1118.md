# UI-09 真实 IME Tab 与三端目视（2026-11-18）

## IME 组字 + 候选 + Tab

`work/probe/ui01_real.xlsx`，交互 RDP 会话，真实微软拼音（HKL `0x08040804`），物理扫描码注入。

1. A7 上按 F2（`0x3C`）进入编辑；
2. 键入拼音 `zhongwen`（z/h/o/n/g/w/e/n）；
3. Space（`0x39`）选中首候选：

```text
编辑栏 = 中文
单元格模式 = 输入
```

4. Tab（`0x0F`）：

```text
名称框 = B7
B7，已选中
A7，中文，已选中   （回读）
```

候选已确认后 Tab 提交“中文”并向右一格，未吞字、未多跳。

同时实测了**未选候选**时直接 Tab 的路径：A2 缓冲区提交为 `zhong'wen`，Tab 后到 B2。该路径的行为与微软拼音自身的原始组合串一致，不把它当作候选确认结果；本项以“候选确认后 Tab”作为 UI-09 的提交行为验收。

## 三端目视

同一份 `ui01_real.xlsx` 分别由本应用、Excel 365、WPS 打开，A1 选中，100% 缩放：

- `ui09-app-a1-1118.png`：本应用，字体“思源黑体 11”，A1 绿色选区，公式栏 123。
- `ui09-excel-1118.png`：Excel 365，A1 选中，公式栏 123；字体为等线系。
- `ui09-wps-1118.png`：WPS Office，A1 选中，公式栏 123。

观察结果：

- 选区：三端均只高亮 A1，边框位置一致，无错格。
- 字体：本应用用内嵌思源黑体（SIL OFL），Excel/WPS 用系统等线/宋体；字形和字符宽度有可见差异。该差异是字体资源差异，归 UI-14 的字体策略，不在 UI-09 伪装成一致。
- 翻页重叠：沿用已验收的 `ui09-page-viewport.txt`（720/900/1080）与 `ui09-shortcuts.txt`；本轮只补目视截图，不改变结论。

## 结论

UI-09 剩余的中文 IME 提交行为与三端目视已执行并留证；导航、选区、PageUp/Down、Ctrl+PageUp/Down 等切片保持已验收。故 UI-09 父项收口。

## 证据

- `docs/excel-wps-audit-2026-11-12/implementation-evidence/ui09-app-a1-1118.png`
- `docs/excel-wps-audit-2026-11-12/implementation-evidence/ui09-excel-1118.png`
- `docs/excel-wps-audit-2026-11-12/implementation-evidence/ui09-wps-1118.png`
- `docs/excel-wps-audit-2026-11-12/implementation-evidence/ui09-app-1118.png`
- `work/probe/ui01_real.xlsx`
