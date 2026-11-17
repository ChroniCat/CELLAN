$ErrorActionPreference='Stop'
$tmp = $env:TEMP
$files = @{
  theme   = Join-Path $tmp 'sc_c18_theme_edit.xlsx'
  pattern = Join-Path $tmp 'sc_c18_pattern_rt.xlsx'
  unknown = Join-Path $tmp 'sc_c18_unknown_edit.xlsx'
}
function Probe($progId, $label) {
  $app = New-Object -ComObject $progId
  try {
    $app.Visible = $false; $app.DisplayAlerts = $false
  } catch {}
  foreach ($k in $files.Keys) {
    $p = $files[$k]
    if (-not (Test-Path $p)) { "$label $k MISSING $p"; continue }
    $wb = $app.Workbooks.Open($p)
    try {
      $ws = $wb.Worksheets.Item(1)
      $a = $ws.Range('A1')
      if ($k -eq 'theme') {
        "{0} theme FONT_THEME={1} FONT_TINT={2} FILL_THEME={3} FILL_TINT={4} BOLD={5}" -f $label,
          $a.Font.ThemeColor, $a.Font.TintAndShade, $a.Interior.ThemeColor, $a.Interior.TintAndShade, $a.Font.Bold
      } elseif ($k -eq 'pattern') {
        "{0} pattern PATTERN={1} DIAG_DOWN={2} DIAG_COLOR={3:X6} FMT={4}" -f $label,
          $a.Interior.Pattern, $a.Borders(5).LineStyle, ($a.Borders(5).Color -band 0xFFFFFF), $a.NumberFormat
      } else {
        $b = $ws.Range('B1')
        "{0} unknown PREFIX=[{1}] B1=[{2}] B1FMT=[{3}]" -f $label, $a.PrefixCharacter, $b.Text, $b.NumberFormat
      }
    } finally { $wb.Close($false) }
  }
  $app.Quit()
}
Probe 'Excel.Application' 'EXCEL'
Probe 'KET.Application' 'WPS'
