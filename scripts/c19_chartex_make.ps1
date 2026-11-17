param([string]$Out)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Add()
$ws=$wb.Worksheets.Item(1); $ws.Name='数据'
$ws.Range('A1').Value2='阶段'; $ws.Range('B1').Value2='金额'
$vals=@('启动','增长','回落','收尾')
$nums=@(100,50,-30,20)
# Excel 的 Range.Value2 走 late binding 时逐格赋 int 会 InvalidCast，改用 object[,] 一次性写
$data=New-Object 'object[,]' 4,2
for($i=0;$i -lt 4;$i++){ $data[$i,0]=$vals[$i]; $data[$i,1]=[double]$nums[$i] }
$ws.Range('A2:B5').Value2=$data
$co=$ws.ChartObjects().Add(200,10,360,240)
try { $co.Chart.ChartType=84; 'set 84 ok' } catch { 'set 84 fail: '+$_.Exception.Message }
if(Test-Path $Out){Remove-Item $Out -Force}
$wb.SaveAs($Out,51)
"charts={0} type={1}" -f $ws.ChartObjects().Count, $co.Chart.ChartType
$wb.Close($false); $xl.Quit()
