param([string]$Path, [string]$Range = 'A1:E3', [string]$Tag = '')
$ErrorActionPreference = 'Stop'
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
  $wb = $xl.Workbooks.Open($Path, 0, $true)
  $ws = $wb.Worksheets.Item(1)
  $r = $ws.Range($Range)
  $rows = $r.Rows.Count
  $cols = $r.Columns.Count
  for ($i = 1; $i -le $rows; $i++) {
    for ($j = 1; $j -le $cols; $j++) {
      $c = $r.Cells.Item($i, $j)
      $addr = $c.Address($false, $false)
      $v = $c.Value2
      if ($null -eq $v) { $v = '' }
      $f = $c.Formula
      $fmt = $c.NumberFormat
      $bold = [int]$c.Font.Bold
      $fill = [int]$c.Interior.Color
      $merged = [int]$c.MergeCells
      "{0}`t{1}`t{2}`t{3}`t{4}`t{5}`t{6}" -f $addr, $v, $f, $fmt, $bold, $fill, $merged
    }
  }
  $wb.Close($false)
} finally {
  $xl.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
