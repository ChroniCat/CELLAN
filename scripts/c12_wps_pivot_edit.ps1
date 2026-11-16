# C12.4：WPS 里改透视表字段再存回（KET.Application）
param(
    [string]$Src = 'F:/finance-native-sheets-ready/work/probe/c12/pivot-real.xlsx',
    [string]$Out = 'F:/finance-native-sheets-ready/work/probe/c12/wps-edited.xlsx'
)
$ErrorActionPreference = 'Stop'
if (Test-Path $Out) { Remove-Item $Out -Force }
$src = $Src.Replace('/', [IO.Path]::DirectorySeparatorChar)
$out = $Out.Replace('/', [IO.Path]::DirectorySeparatorChar)

$app = New-Object -ComObject KET.Application
$app.Visible = $false
$app.DisplayAlerts = $false
$wb = $app.Workbooks.Open($src)
"WPS sheets=$($wb.Worksheets.Count)"
$found = 0
foreach ($ws in $wb.Worksheets) {
    try { $pts = $ws.PivotTables() } catch { continue }
    $n = 0
    try { $n = $pts.Count } catch { $n = 0 }
    if ($n -gt 0) {
        "sheet=$($ws.Name) pivots=$n"
        foreach ($pt in $pts) {
            $found++
            try { "  pt=$($pt.Name) rowFields=$(@($pt.RowFields) | ForEach-Object { $_.Name })" } catch { "  pt=$($pt.Name) rowFields=<err>" }
            try { "  colFields=$(@($pt.ColumnFields) | ForEach-Object { $_.Name })" } catch { "  colFields=<err>" }
        }
    }
}
if ($found -eq 0) {
    "NO_PIVOT_VISIBLE"
    $wb.Close($false)
    $app.Quit()
    exit 0
}
# 尝试把行字段改成列字段（第一张表的第一个透视表）
$ws = $wb.Worksheets.Item(1)
$pt = $ws.PivotTables().Item(1)
$fieldName = $null
# WPS 的 RowFields 集合枚举不可靠（返回集合本身），改用 PivotFields 索引定位行字段
try {
    for ($i = 1; $i -le $pt.PivotFields().Count; $i++) {
        $pf = $pt.PivotFields($i)
        if ($pf.Orientation -eq 1) { $fieldName = $pf.Name; break }
    }
} catch { "scan_err $($_.Exception.Message)" }
"target_field=$fieldName"
$changed = 'no'
if ($fieldName) {
    try {
        for ($i = 1; $i -le $pt.PivotFields().Count; $i++) {
            $pf = $pt.PivotFields($i)
            if ($pf.Name -eq $fieldName) {
                $pf.Orientation = 2   # xlColumnField
                $changed = 'yes'
                break
            }
        }
    } catch { $changed = 'ERR ' + $_.Exception.Message }
}
"changed=$changed"
try { $wb.SaveAs($out, 51) } catch { "SAVE_ERR $($_.Exception.Message)" }
$wb.Close($false)
$app.Quit()
"OUT=$out"
