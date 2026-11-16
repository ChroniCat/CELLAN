# Excel 自己把大纲转压缩（RowAxisLayout(1)）当基准，并导出两个文件的 UsedRange
param(
    [string]$Our = 'F:/finance-native-sheets-ready/work/probe/c12/compact-ours.xlsx',
    [string]$Ref = 'F:/finance-native-sheets-ready/work/probe/c12/ref-compact.xlsx',
    [string]$OutDir = 'F:/finance-native-sheets-ready/work/probe/c12/layout'
)
$ErrorActionPreference = 'Stop'
$our = $Our.Replace('/', [IO.Path]::DirectorySeparatorChar)
$ref = $Ref.Replace('/', [IO.Path]::DirectorySeparatorChar)
$dir = $OutDir.Replace('/', [IO.Path]::DirectorySeparatorChar)
New-Item -ItemType Directory -Force -Path $dir | Out-Null
if (Test-Path $ref) { Remove-Item $ref -Force }

function Dump-Range($path, $tsv) {
    $xl = New-Object -ComObject Excel.Application
    $xl.Visible = $false
    $xl.DisplayAlerts = $false
    $wb = $xl.Workbooks.Open($path, 0, $true)
    Start-Sleep -Milliseconds 1200
    $ws = $wb.Worksheets.Item(1)
    $rng = $ws.UsedRange
    $rows = $rng.Rows.Count
    $cols = $rng.Columns.Count
    $lines = @()
    for ($r = 1; $r -le $rows; $r++) {
        $cells = @()
        for ($c = 1; $c -le $cols; $c++) {
            $v = $rng.Cells.Item($r, $c).Text
            if ($null -eq $v) { $v = '' }
            $cells += [string]$v
        }
        $lines += ($cells -join "`t")
    }
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllLines($tsv, $lines, $utf8)
    "$(Split-Path $tsv -Leaf): rows=$rows cols=$cols"
    $wb.Close($false)
    $xl.Quit()
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}

# 1) 让 Excel 打开我们的文件、切压缩布局、另存为基准
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($our)
Start-Sleep -Milliseconds 800
$pt = $wb.Worksheets.Item(1).PivotTables(1)
$pt.RowAxisLayout(1)     # xlCompactRow
$wb.SaveAs($ref, 51)
$wb.Close($false)
$xl.Quit()
[void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
"REF_SAVED=$ref"

Dump-Range $our (Join-Path $dir 'ours.tsv')
Dump-Range $ref (Join-Path $dir 'ref.tsv')
