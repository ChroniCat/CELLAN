param([string]$Out = 'F:\finance-native-sheets-ready\work\probe\qa042\excel-pasted.xlsx')
$ErrorActionPreference = 'Stop'
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
  $wb = $xl.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Range('A1').Select()
  try {
    $ws.Paste() | Out-Null
    Start-Sleep -Milliseconds 400
    "paste=ok A1=$($ws.Range('A1').Value2) D1f=$($ws.Range('D1').Formula) C1fmt=$($ws.Range('C1').NumberFormat)"
    $wb.SaveAs($Out, 51)
  } catch {
    "paste=FAIL $($_.Exception.Message)"
  }
  $wb.Close($false)
} finally {
  $xl.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
