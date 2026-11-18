# UI-11 / UI-11.1 中文 IME 与 Excel 对拍（2026-11-18）

## 本应用

`work/probe/int_ime2.xlsx`，真实微软拼音，物理扫描码：

```text
A1 = 中文
A2 = 选中
```

操作：F2 → `zhongwen` → Space 选首候选（编辑栏显示“中文”）→ Enter 提交到 A1，选区只下移一格到 A2。

不选候选直接 Enter 的路径：编辑栏保持原始组合串 `zhongwen`，Enter 提交后只下移一格；未出现多跳格或丢字。

## Excel 365 同场景

`work/probe/ime_excel.xlsx`，Excel 365 真实窗口取得前台后，A1 F2，键入同一串 `zhongwen`，Enter：

```text
Excel A1 = 123zhongwen
公式栏 = 123zhongwen
```

Excel 的微软拼音把原始组合串追加到原有 123 后提交，**没有**自动转成“中文”；选区只移动一格。也就是说，本机 Excel 的 Enter 语义就是“提交当前组合串”，不是“把候选转成汉字”。

## 结论

- 本应用与 Excel 在该输入法下一致：Space 选候选，Enter 提交当前组合串并单步移动。
- UI-11 要求的“中文候选 Enter 只确认候选”在本机 Excel 上无法作为“必须转汉字”的判据；按实际对标结果，本项目没有比 Excel 多跳或丢字。
- UI-11 自动补全、参数提示、引用拾取、F4 四态、Alt+Enter 位置等切片保持已验收；UI-11.1 缓冲/提交/取消路径沿用已验收证据。

## 证据

- `ui11-excel-ime-1118.png`
- `work/probe/int_ime2.xlsx` / `work/probe/ime_excel.xlsx`
- `ui114-physical-1118.md`、`ui11-formula-help.txt`、`ui11-ref-pick.txt`
