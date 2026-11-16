# 让 Excel 自己造一份「地区(行) x 产品(列) / 求和:金额」的透视表，保存成 xlsx，
# 用来当结构黄金样本（对比我们生成的 pivotCacheDefinition / pivotTable XML）。
$ErrorActionPreference = 'Stop'
$out = $args[0]
if ([string]::IsNullOrWhiteSpace($out)) { $out = 'F:/finance-native-sheets-ready/work/probe/c12/excel-made.xlsx' }

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false; $xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Add()
$ws = $wb.Worksheets.Item(1)
$ws.Name = 'Data'
$ws.Cells.Item(1,1) = '地区'; $ws.Cells.Item(1,2) = '产品'; $ws.Cells.Item(1,3) = '金额'
$rows = @(
  @('华东','A',10), @('华北','B',20), @('华东','A',30),
  @('华北','B',40), @('华东','B',50), @('华北','A',60)
)
for ($i = 0; $i -lt $rows.Count; $i++) {
  $r = $i + 2
  $ws.Cells.Item($r,1) = $rows[$i][0]
  $ws.Cells.Item($r,2) = $rows[$i][1]
  $ws.Cells.Item($r,3) = [double]$rows[$i][2]
}
$src = $ws.Range('A1:C7')
$pc = $wb.PivotCaches().Create(1, $src)   # xlDatabase = 1
$pt = $pc.CreatePivotTable($ws.Range('E3'), 'PT1')
$pt.PivotFields('地区').Orientation = 1   # xlRowField
$pt.PivotFields('产品').Orientation = 2   # xlColumnField
$pt.AddDataField($pt.PivotFields('金额'), '求和项:金额', -4157) | Out-Null  # xlSum
$pt.RowAxisLayout(1) | Out-Null
"version=" + $xl.Version
"range=" + $pt.TableRange1.Address($false,$false)
$outFull = [IO.Path]::GetFullPath($out).Replace('/','')
try { $wb.SaveAs($outFull, 51) } catch { 'saveas failed: ' + $_.Exception.Message; 'inner: ' + $_.Exception.InnerException.Message }
$wb.Close($false)
$xl.Quit()
"saved " + $outFull
