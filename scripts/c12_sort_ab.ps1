$ErrorActionPreference='Stop'
function Make($variant){
  Get-Process EXCEL -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Milliseconds 400
  $xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
  $wb=$xl.Workbooks.Add(); $ws=$wb.Worksheets.Item(1); $ws.Name='Data'
  $rows=@(@('地区','金额'),@('深圳',10),@('上海',20),@('杭州',30),@('广州',40),@('北京',50),@('北京',60))
  $arr=New-Object 'object[,]' 7,2
  for($r=0;$r -lt 7;$r++){for($c=0;$c -lt 2;$c++){$arr[$r,$c]=$rows[$r][$c]}}
  $ws.Range('A1:B7').Value2=$arr
  $ws2=$wb.Worksheets.Add([System.Reflection.Missing]::Value,$ws); $ws2.Name='透视'
  $pc=$wb.PivotCaches().Create(1,$ws.Range('A1:B7'))
  $pt=$pc.CreatePivotTable($ws2.Range('A3'),'PT1')
  if($variant -eq 'A'){
    $pt.RowAxisLayout(1)|Out-Null
    $pt.PivotFields('地区').Orientation=1
    $pt.AddDataField($pt.PivotFields('金额'),'求和项:金额',-4157)|Out-Null
  } elseif($variant -eq 'B'){
    $pt.PivotFields('地区').Orientation=1
    $pt.AddDataField($pt.PivotFields('金额'),'求和项:金额',-4157)|Out-Null
    $pt.RowAxisLayout(1)|Out-Null
  } else {
    $pt.RowAxisLayout(1)|Out-Null
    $pt.PivotFields('地区').Orientation=1
    $pt.AddDataField($pt.PivotFields('金额'),'求和项:金额',-4157)|Out-Null
    $pf=$pt.PivotFields('地区')
    $pf.AutoSort(1,'求和项:金额')
  }
  $pt.RefreshTable()
  $used=$ws2.UsedRange; $out=@()
  for($r=1;$r -le $used.Rows.Count;$r++){ $out += [string]$ws2.Cells.Item($r,1).Text }
  $txt = ($out -join ' | ')
  $wb.Close($false); $xl.Quit()
  return "$variant : $txt"
}
"== A"; Make 'A'
"== B"; Make 'B'
"== C"; Make 'C'
