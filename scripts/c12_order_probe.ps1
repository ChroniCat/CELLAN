param([string]$Path)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($Path)
foreach($ws in @($wb.Worksheets)){
  $n=$ws.PivotTables().Count
  if($n -gt 0){
    foreach($pt in @($ws.PivotTables())){
      $rf=$pt.RowFields(1)
      $names=@()
      foreach($it in @($rf.PivotItems())){ $names += $it.Name }
      "sheet={0} table={1} rows=[{2}]" -f $ws.Name,$pt.Name,($names -join ',')
    }
  }
}
$wb.Close($false); $xl.Quit()
