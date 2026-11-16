# QA-01.3 图形矩阵探针：组合/连接线/预设几何/内嵌图表
$ErrorActionPreference='Stop'
$file=(Resolve-Path $args[0]).Path
$prog= if ($args.Count -ge 2) { $args[1] } else { 'Excel.Application' }
$xl=New-Object -ComObject $prog
$xl.Visible=$false
try { $xl.DisplayAlerts=$false } catch {}
$wb=$xl.Workbooks.Open($file,0,$true)
try {
  "product=" + $xl.Name + " " + $xl.Version
  $ws=$wb.Worksheets.Item(1)
  "shapes=" + $ws.Shapes.Count
  "charts=" + $ws.ChartObjects().Count
  for ($i=1; $i -le $ws.Shapes.Count; $i++) {
    $s=$ws.Shapes.Item($i)
    $line = "shape" + $i + "=type" + $s.Type + " name=" + $s.Name + " w=" + [int]$s.Width + " h=" + [int]$s.Height
    if ($s.Type -eq 6) { $line += " items=" + $s.GroupItems.Count }
    if ($s.Type -eq 13) { $line += " (picture)" }
    "  " + $line
  }
  for ($c=1; $c -le $ws.ChartObjects().Count; $c++) {
    $co=$ws.ChartObjects($c)
    $t=''
    try { $t=$co.Chart.ChartTitle.Text } catch { $t='(no title)' }
    "chart" + $c + "=name:" + $co.Name + " series:" + $co.Chart.SeriesCollection().Count + " title:" + $t + " w=" + [int]$co.Width + " h=" + [int]$co.Height
  }
} finally { $wb.Close($false); $xl.Quit() }
