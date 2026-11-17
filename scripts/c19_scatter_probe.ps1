param([string]$Path)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($Path); $ws=$wb.Worksheets.Item('散点')
$ch=$ws.ChartObjects(1).Chart
"type={0}" -f $ch.ChartType
foreach($se in @($ch.SeriesCollection())){
  $xs=@($se.XValues); $ys=@($se.Values)
  "series={0} n={1} X=[{2}] Y=[{3}]" -f $se.Name,$xs.Count,($xs -join ','),($ys -join ',')
}
$wb.Close($false); $xl.Quit()
