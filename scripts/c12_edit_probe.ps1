# C12.4 可编辑性：用 Excel 打开我们生成的透视表，改字段（加一个最大值数据字段），另存。
# 之后再用 pivot_roundtrip 走一遍我们的保存，最后用 probe2 在 Excel / WPS 复核。
$ErrorActionPreference = 'Stop'
$src = (Resolve-Path $args[0]).Path.Replace('/', '\')
$dst = [IO.Path]::GetFullPath($args[1]).Replace('/', '\')

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($src, 0, $false)
$ws = $wb.Worksheets.Item('Data')
$pt = $ws.PivotTables(1)
$before = @($pt.DataFields() | ForEach-Object { $_.Name })
$pt.AddDataField($pt.PivotFields('金额'), '最大值金额', -4136) | Out-Null
$pt.RefreshTable() | Out-Null
$after = @($pt.DataFields() | ForEach-Object { $_.Name })
$wb.SaveAs($dst, 51)
$wb.Close($false)
$xl.Quit()
"before=" + ($before -join '|')
"after=" + ($after -join '|')
"saved " + $dst
