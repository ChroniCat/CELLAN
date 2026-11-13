$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\work\goal-logs\gui\c17-excel-borders.xlsx"
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1").Value2 = 1
    $ws.Range("A2").Value2 = 2
    $r = $ws.Range("A1:A2")
    $fc = $r.FormatConditions.Add(2, [System.Reflection.Missing]::Value, "=A1>0")
    $fc.Borders.LineStyle = 1
    $fc.Borders.Item(7).Color = 255
    $fc.Borders.Item(10).Color = 16711680
    $fc.Borders.Item(8).Color = 65280
    $fc.Borders.Item(9).Color = 65535
    $wb.SaveAs($out)
    $wb.Close($false)
    "SAVED=$out"
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
