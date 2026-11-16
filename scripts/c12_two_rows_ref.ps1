$ErrorActionPreference='Stop'
Get-Process EXCEL -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
$xl = New-Object -ComObject Excel.Application
$xl.Visible=$false; $xl.DisplayAlerts=$false
$wb = $xl.Workbooks.Add()
$ws = $wb.Worksheets.Item(1); $ws.Name='Data'
$data = @(
 @('地区','产品','金额'),
 @('华东','A',10),@('华北','B',20),@('华东','A',30),
 @('华北','B',40),@('华东','B',50),@('华北','A',60)
)
$arr = New-Object 'object[,]' 7,3
for($r=0; $r -lt 7; $r++){ for($c=0; $c -lt 3; $c++){ $arr[$r,$c] = $data[$r][$c] } }
$ws.Range('A1:C7').Value2 = $arr
$ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws); $ws2.Name = '透视'
$pc = $wb.PivotCaches().Create(1, $ws.Range('A1:C7'))
$pt = $pc.CreatePivotTable($ws2.Range('A3'), 'PT1')
$pt.RowAxisLayout(1) | Out-Null
$pt.PivotFields('地区').Orientation = 1
$pt.PivotFields('产品').Orientation = 1
$pt.AddDataField($pt.PivotFields('金额'), '求和项:金额', -4157) | Out-Null
$pt.RefreshTable()
$used = $ws2.UsedRange
$lines = @()
for($r=1; $r -le $used.Rows.Count; $r++){
  $cells=@()
  for($c=1; $c -le $used.Columns.Count; $c++){ $cells += [string]$used.Cells.Item($r,$c).Text }
  $lines += ($cells -join "`t")
}
Set-Content -Path 'F:\finance-native-sheets-ready\work\probe\c12\c12-2-ref.tsv' -Value $lines -Encoding UTF8
$wb.SaveAs('F:\finance-native-sheets-ready\work\probe\c12\c12-2-ref.xlsx', 51)
$wb.Close($false); $xl.Quit()
'rows=' + $lines.Count
