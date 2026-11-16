# 生成 compact="1" 透视表夹具（Excel 自己的默认压缩布局）
param([string]$Out = 'F:/finance-native-sheets-ready/work/probe/c12/compact-src.xlsx')
$ErrorActionPreference = 'Stop'
$out = $Out.Replace('/', [IO.Path]::DirectorySeparatorChar)
if (Test-Path $out) { Remove-Item $out -Force }

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Add()
$ws = $wb.Worksheets.Item(1)
$ws.Name = 'Data'
$data = @(
    @('地区', '产品', '金额'),
    @('华东', 'A', 10), @('华北', 'B', 20), @('华东', 'B', 30),
    @('华北', 'A', 40), @('华东', 'A', 50), @('华北', 'B', 60)
)
for ($r = 0; $r -lt $data.Count; $r++) {
    for ($c = 0; $c -lt 3; $c++) {
        $ws.Cells.Item($r + 1, $c + 1).Value2 = $data[$r][$c]
    }
}
$src = $ws.Range('A1:C7')
$cache = $wb.PivotCaches.Create(1, $src)   # xlDatabase
$pt = $cache.CreatePivotTable($ws.Range('E3'), 'PT1')
$pt.PivotFields('地区').Orientation = 1   # xlRowField
$pt.PivotFields('产品').Orientation = 2   # xlColumnField
$pt.PivotFields('金额').Orientation = 4   # xlDataField
$wb.SaveAs($out, 51)
$wb.Close($false)
$xl.Quit()
[void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
"SAVED=$out"
