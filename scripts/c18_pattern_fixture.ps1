$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c18\pattern.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1").Value2 = "图案填充"
    $ws.Range("A1").Interior.Pattern = -4124          # xlPatternGray25
    $ws.Range("A1").Interior.PatternColorIndex = 1    # 自动(黑)
    $ws.Range("B1").Value2 = "对角线"
    $ws.Range("B1").Borders.Item(5).LineStyle = 1     # xlDiagonalDown, xlContinuous
    $ws.Range("B1").Borders.Item(5).Color = 255       # 红 (BGR)
    $ws.Range("B1").Borders.Item(5).Weight = 2
    $ws.Range("C1").Value2 = 1.5
    $ws.Range("C1").NumberFormat = "0.00%"
    $ws.Range("A1:C1").ColumnWidth = 16
    $wb.SaveAs($out)
    "PATTERN={0} DIAG_DOWN={1}" -f $ws.Range("A1").Interior.Pattern, $ws.Range("B1").Borders.Item(5).LineStyle
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
