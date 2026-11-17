param([string]$Path)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($Path)
foreach($ws in @($wb.Worksheets)){
  $n=$ws.ChartObjects().Count
  $types=@()
  foreach($co in @($ws.ChartObjects())){ $types += $co.Chart.ChartType }
  "{0}: charts={1} types=[{2}]" -f $ws.Name,$n,($types -join ',')
}
$wb.Close($false); $xl.Quit()
