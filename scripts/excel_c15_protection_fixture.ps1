$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c15\protected.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "受保护"
    $ws.Range("A1").Formula = "=1+2"
    $ws.Range("A1").Locked = $true
    $ws.Range("A1").FormulaHidden = $true
    $ws.Range("B1").Locked = $false
    $ws.Range("B1").Value2 = "输入区"
    $ws.Range("A3").Value2 = 10
    $ws.Range("A4").Value2 = 2
    $ws.Range("A5").Value2 = 7
    # 只允许「设置单元格格式」：Password, DrawingObjects, Contents, Scenarios, UIOnly, AllowFormattingCells...
    $ws.Protect("", $false, $true, $false, $false, $true, $false, $false, $false, $false, $false, $false, $false, $false, $false)
    $wb.SaveAs($out, 51)
    "SAVED=$out"
    "A1_LOCKED=" + $ws.Range("A1").Locked + " A1_HIDDEN=" + $ws.Range("A1").FormulaHidden
    "B1_LOCKED=" + $ws.Range("B1").Locked
    # 只许格式：改锁定格格式应成功
    try { $ws.Range("A3").Font.Bold = $true; "FMT_LOCKED=ok" } catch { "FMT_LOCKED=denied" }
    # 排序应被拒
    try { $ws.Range("A3:A5").Sort($ws.Range("A3")) | Out-Null; "SORT=ok" } catch { "SORT=denied" }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
