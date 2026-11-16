$ErrorActionPreference='Stop'
Get-Process EXCEL -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
$xl = New-Object -ComObject Excel.Application
$xl.Visible=$false; $xl.DisplayAlerts=$false
$wb = $xl.Workbooks.Add()
$ws = $wb.Worksheets.Item(1); $ws.Name='Data'
# 源顺序刻意打乱：深圳 上海 杭州 广州 北京；第二列用 销售/行政
$rows = @(
  @('地区','部门','金额'),
  @('深圳','销售',10), @('上海','行政',20), @('杭州','销售',30),
  @('广州','行政',40), @('北京','销售',50), @('北京','行政',60)
)
$arr = New-Object 'object[,]' 7,3
for($r=0; $r -lt 7; $r++){ for($c=0; $c -lt 3; $c++){ $arr[$r,$c] = $rows[$r][$c] } }
$ws.Range('A1:C7').Value2 = $arr
$ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws); $ws2.Name='透视'
$pc = $wb.PivotCaches().Create(1, $ws.Range('A1:C7'))
$pt = $pc.CreatePivotTable($ws2.Range('A3'), 'PT1')
$pt.RowAxisLayout(1) | Out-Null
$pt.PivotFields('地区').Orientation = 1
$pt.PivotFields('部门').Orientation = 1
$pt.AddDataField($pt.PivotFields('金额'), '求和项:金额', -4157) | Out-Null
$pt.RefreshTable()
$used = $ws2.UsedRange
$lines=@()
for($r=1; $r -le $used.Rows.Count; $r++){
  $cells=@()
  for($c=1; $c -le $used.Columns.Count; $c++){ $cells += [string]$used.Cells.Item($r,$c).Text }
  $lines += ($cells -join "`t")
}
Set-Content -Path 'F:\finance-native-sheets-ready\work\probe\c12\sort-ref.tsv' -Value $lines -Encoding UTF8
$wb.Close($false); $xl.Quit()
'OK'
