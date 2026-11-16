param([string]$Path)
$ErrorActionPreference = 'Stop'
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
  $wb = $xl.Workbooks.Open($Path, 0, $true)
  "title=$($wb.Name)"
  "sheets=$($wb.Worksheets.Count)"
  $ws = $wb.Worksheets.Item(1)
  "A1=$($ws.Range('A1').Value2)"
  "B2=$($ws.Range('B2').Value2)"
  "B3formula=$($ws.Range('B3').Formula)"
  "used=$($ws.UsedRange.Rows.Count)x$($ws.UsedRange.Columns.Count)"
  "name_has_fixed=$($wb.Name -match '修复')"
  $wb.Close($false)
} finally {
  $xl.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
