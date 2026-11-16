# QA-06.1：SUBTOTAL/AGGREGATE 隐藏行口径 —— Excel 实算导出
# 用法：powershell -File scripts/qa06_subtotal_probe.ps1 F:/.../work/qa06h
param([string]$Dir = 'work/qa06h')
$ErrorActionPreference = 'Stop'
$Dir = $Dir.Replace('/', [IO.Path]::DirectorySeparatorChar)
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$file = Join-Path $Dir 'subtotal.xlsx'
if (Test-Path $file) { Remove-Item $file -Force }

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Add()
while ($wb.Worksheets.Count -lt 2) { $wb.Worksheets.Add() | Out-Null }
$ws = $wb.Worksheets.Item(1)
$ws.Name = 'S1'
$ws2 = $wb.Worksheets.Item(2)
$ws2.Name = 'S2'
$ws2.Range('A1').Value2 = 100
for ($i = 1; $i -le 10; $i++) { $ws.Cells.Item($i, 1).Value2 = $i }
# 手动隐藏第 3、5 行
$ws.Rows.Item(3).EntireRow.Hidden = $true
$ws.Rows.Item(5).EntireRow.Hidden = $true

$cases = @(
    @('C1', '=SUBTOTAL(9,A1:A10)'),
    @('C2', '=SUBTOTAL(109,A1:A10)'),
    @('C3', '=SUBTOTAL(3,A1:A10)'),
    @('C4', '=AGGREGATE(9,5,A1:A10)'),
    @('C5', '=SUM(A:A)'),
    @('C6', "=S2!A1+1"),
    @('C7', '=SUBTOTAL(1,A1:A10)')
)
foreach ($c in $cases) { $ws.Range($c[0]).Formula = $c[1] }
$ws.Calculate()

$lines = @()
foreach ($c in $cases) {
    $cell = $ws.Range($c[0])
    $v = $cell.Value2
    if ($null -eq $v) { $v = '' }
    $lines += (($c[0], $c[1], [string]$cell.Text) -join "`t")
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllLines((Join-Path $Dir 'subtotal.tsv'), $lines, $utf8)

$meta = "Excel $($xl.Version) hidden=3,5 values=1..10"
[IO.File]::WriteAllText((Join-Path $Dir 'probe-meta.txt'), $meta, $utf8)
$wb.Close($false)
$xl.Quit()
"EXCEL $($xl.Version)：$($lines.Count) 条"
$lines
