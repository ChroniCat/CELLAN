param([string]$Dir = 'F:\finance-native-sheets-ready\work\probe\qa042')
$ErrorActionPreference = 'Stop'
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open((Join-Path $Dir 'fixture.xlsx'), 0, $true)
$ws = $wb.Worksheets.Item(1)
$ws.Range('A1:E3').Copy() | Out-Null
Start-Sleep -Milliseconds 500

$et = New-Object -ComObject KET.Application
$et.Visible = $false
$et.DisplayAlerts = $false
try {
  $w2 = $et.Workbooks.Add()
  $s2 = $w2.Worksheets.Item(1)
  $s2.Range('A1').Select()
  try {
    $s2.Paste() | Out-Null
    Start-Sleep -Milliseconds 500
    "wps_from_excel=ok A1=$($s2.Range('A1').Value2) D1f=$($s2.Range('D1').Formula) C1fmt=$($s2.Range('C1').NumberFormat)"
    $w2.SaveAs((Join-Path $Dir 'wps-from-excel.xlsx'), 51)
  } catch { "wps_from_excel=FAIL $($_.Exception.Message)" }
  $w2.Close($false)
} finally {
  $et.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($et)
  $wb.Close($false)
  $xl.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
