# C12.4：用 Excel 打开带真 pivotTable 部件的文件，确认是可编辑透视表（不是值化网格）。
# 输出：sheet / pivot 数量 / 名称 / 行字段 / 列字段 / 数据字段 / 表范围 / 刷新后前几格的值
$ErrorActionPreference = 'Stop'
$file = $args[0]
if ([string]::IsNullOrWhiteSpace($file)) { $file = 'work/probe/c12/pivot-real.xlsx' }
$file = (Resolve-Path $file).Path
$appName = if ($args.Count -gt 1) { $args[1] } else { 'Excel.Application' }

$xl = New-Object -ComObject $appName
$xl.Visible = $false; $xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($file, 0, $false)

"app=" + $appName + " version=" + $xl.Version
$total = 0
foreach ($ws in $wb.Worksheets) {
    $pts = $ws.PivotTables()
    $count = @($pts).Count
    "{0}: PivotTables={1}" -f $ws.Name, $count
    foreach ($pt in $pts) {
        $total++
        $rowFields = @()
        foreach ($f in $pt.RowFields()) { $rowFields += $f.Name }
        $colFields = @()
        foreach ($f in $pt.ColumnFields()) { $colFields += $f.Name }
        $dataFields = @()
        foreach ($f in $pt.DataFields()) { $dataFields += $f.Name }
        "  name=$($pt.Name) rows=[$($rowFields -join ',')] cols=[$($colFields -join ',')] data=[$($dataFields -join ',')]"
        "  range=$($pt.TableRange1.Address($false,$false)) cache=$($pt.PivotCache().SourceData)"
        try { $pt.RefreshTable() } catch { "  refresh failed: $($_.Exception.Message)" }
        $rng = $pt.TableRange1
        $vals = @()
        for ($r = 1; $r -le [Math]::Min(6, $rng.Rows.Count); $r++) {
            $line = @()
            for ($c = 1; $c -le $rng.Columns.Count; $c++) {
                $cell = $rng.Cells.Item($r, $c)
                $t = [string]$cell.Text
                $line += $t
            }
            $vals += ($line -join '|')
        }
        "  values:"
        $vals | ForEach-Object { "    $_" }
    }
}
"total=" + $total
$wb.Close($false)
$xl.Quit()
