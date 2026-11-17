# UI-26.1：份数交给驱动（一个作业 Copies=N）实测（2026-11-17）

## 问题

旧实现 `print_to_printer` 对份数循环：

```
for _ in 0..copies { Start-Process -FilePath x.pdf -Verb PrintTo ... }
```

这会向队列提交 **N 个单份作业**，驱动的 DEVMODE 里 `dmCopies` 仍是 1；验收要求
「份数按 N 个打印任务提交，未向驱动传 Copies」不成立。

## 修正

新增 `scripts/print_pdf.ps1`，由应用用 `include_str!` 内嵌并在打印时落到临时目录：

- 用 Windows 内置 `Windows.Data.Pdf` 逐页渲染 PDF 为位图（200dpi）；
- 一个 `System.Drawing.Printing.PrintDocument` 作业，设置
  `PrinterSettings.Copies = N`，由 .NET 写进驱动 DEVMODE；
- 应用只调用一次助手，不再循环 `Start-Process`；
- 空打印机名时由助手取系统默认打印机。

## 端到端实测

为免出纸，建了一个 **NUL 端口 + Canon UFR II 驱动** 的临时队列
`SheetNativeTest2`（真实驱动，支持 Copies，输出丢弃），暂停队列后从 app 走
`SC_PRINT_TO`/`SC_PRINT_COPIES` 钩子（和“打印”按钮同一个 `print_to_printer`）：

```
SC_PRINT_TO=SheetNativeTest2 SC_PRINT_COPIES=3 SC_PRINT_EXIT=1 \
  target/debug/app.exe crates/core/tests/fixtures/qa09/a11y.xlsx

[print] 已提交 1 个打印任务（3 份，1 页）到 SheetNativeTest2：
  C:\Windows\TEMP\sheetcore-print-1794939685.pdf
  （PRINTED=1 COPIES_REQUESTED=3 PAGES=1）

Get-PrintJob -PrinterName SheetNativeTest2
Id           : 9
DocumentName : sheetcore-print-1794939685.pdf
TotalPages   : 1
Size         : 19896
```

关键点：**队列里只有 1 个作业**（旧实现会是 3 个），助手读回
`PrinterSettings.Copies=3`。日志存 `qa26-print-job.log`；测试队列已删除。

## 仍缺

- 真实打印机出纸/份数肉眼核对（本机有 Canon iR C3120L/C3322L 队列，但不能
  在无人值守时出纸；需要你允许或到机器旁确认）；
- 真实鼠标拖滑块/改份数；
- WPS 页面数以外的 PDF 像素对拍。

因此 UI-26.1 仍待验收，但「N 个单份作业」这条已改成「1 个 Copies=N 作业」并有
队列级证据。
