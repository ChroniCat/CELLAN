param([string]$Dir)
$ErrorActionPreference='Continue'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Add()
$ws=$wb.Worksheets.Item(1); $ws.Name='数据'
$ws.Range('A1').Value2='阶段'; $ws.Range('B1').Value2='金额'
$vals=@('启动','增长','回落','收尾')
$nums=@(100,50,-30,20)
$data=New-Object 'object[,]' 4,2
for($i=0;$i -lt 4;$i++){ $data[$i,0]=$vals[$i]; $data[$i,1]=[double]$nums[$i] }
$ws.Range('A2:B5').Value2=$data
foreach($t in 84,117,118,119,121,122,123){
  $co=$ws.ChartObjects().Add(200,10,360,240)
  try {
    $co.Chart.ChartType=$t
    $real=$co.Chart.ChartType
    $out=Join-Path $Dir ("chartex-{0}.xlsx" -f $t)
    if(Test-Path $out){Remove-Item $out -Force}
    $wb.SaveAs($out,51)
    "type={0} set=ok real={1} file={2}" -f $t,$real,(Split-Path $out -Leaf)
  } catch {
    "type={0} set=fail {1}" -f $t,$_.Exception.Message
  }
  $co.Delete()
}
$wb.Close($false); $xl.Quit()
