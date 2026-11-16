# C12.4 显示值方式：用 Excel 在已有透视表上切「占总计百分比」，导出单元格值
param(
    [string]$Src = 'F:/finance-native-sheets-ready/work/probe/c12/regen/row_col.xlsx',
    [string]$Out = 'F:/finance-native-sheets-ready/work/probe/c12/show-pct.xlsx',
    [int]$Calc = 5
)
$ErrorActionPreference = 'Stop'
$src = $Src.Replace('/', [IO.Path]::DirectorySeparatorChar)
$out = $Out.Replace('/', [IO.Path]::DirectorySeparatorChar)
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($src)
Start-Sleep -Milliseconds 1000
$ws = $wb.Worksheets.Item(1)
$pt = $ws.PivotTables(1)
$df = $pt.DataFields(1)
"before calc=" + $df.Calculation
$df.Calculation = $Calc
$df.NumberFormat = "0.00%"
$ws.Calculate()
$rng = $pt.TableRange1
"after calc=" + $df.Calculation + " range=" + $rng.Address($false, $false)
for ($r = 1; $r -le $rng.Rows.Count; $r++) {
    $line = @()
    for ($c = 1; $c -le $rng.Columns.Count; $c++) { $line += [string]$rng.Cells.Item($r, $c).Text }
    $line -join "`t"
}
$wb.SaveAs($out, 51)
$wb.Close($false)
$xl.Quit()
[void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
"SAVED=$out"
