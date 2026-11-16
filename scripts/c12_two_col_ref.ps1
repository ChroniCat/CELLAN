$ErrorActionPreference='Stop'
Get-Process EXCEL -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Add(); $ws=$wb.Worksheets.Item(1); $ws.Name='Data'
$rows=@(
 @('地区','产品','状态','金额'),
 @('华东','A','有效',10),@('华北','B','有效',20),@('华东','A','无效',30),
 @('华北','B','有效',40),@('华东','B','无效',50),@('华北','A','有效',60)
)
$arr=New-Object 'object[,]' 7,4
for($r=0;$r -lt 7;$r++){for($c=0;$c -lt 4;$c++){$arr[$r,$c]=$rows[$r][$c]}}
$ws.Range('A1:D7').Value2=$arr
$ws2=$wb.Worksheets.Add([System.Reflection.Missing]::Value,$ws); $ws2.Name='透视'
$pc=$wb.PivotCaches().Create(1,$ws.Range('A1:D7')); $pt=$pc.CreatePivotTable($ws2.Range('A3'),'PT1')
$pt.RowAxisLayout(1)|Out-Null
$pt.PivotFields('地区').Orientation=1
$pt.PivotFields('产品').Orientation=2
$pt.PivotFields('状态').Orientation=2
$pt.AddDataField($pt.PivotFields('金额'),'求和项:金额',-4157)|Out-Null
$pt.RefreshTable()
$used=$ws2.UsedRange; $lines=@()
for($r=1;$r -le $used.Rows.Count;$r++){$c=@();for($i=1;$i -le $used.Columns.Count;$i++){$c += [string]$used.Cells.Item($r,$i).Text};$lines+=($c -join "`t")}
Set-Content -Path 'F:\finance-native-sheets-ready\work\probe\c12\twocol-ref.tsv' -Value $lines -Encoding UTF8
$wb.Close($false); $xl.Quit(); 'OK'
