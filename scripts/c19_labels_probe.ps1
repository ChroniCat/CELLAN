param([string]$Path)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($Path)
$ws=$wb.Worksheets.Item(1)
"charts={0}" -f $ws.ChartObjects().Count
foreach($co in @($ws.ChartObjects())){
  $ch=$co.Chart
  foreach($se in @($ch.SeriesCollection())){
    $has=$se.HasDataLabels
    if($has){
      $dl=$se.DataLabels()
      "series={0} HAS=1 ShowValue={1} ShowCategoryName={2} ShowSeriesName={3} ShowPercent={4} ShowLegendKey={5} Position={6}" -f `
        $se.Name,$dl.ShowValue,$dl.ShowCategoryName,$dl.ShowSeriesName,$dl.ShowPercent,$dl.ShowLegendKey,$dl.Position
    } else {
      "series={0} HAS=0" -f $se.Name
    }
  }
}
$wb.Close($false); $xl.Quit()
