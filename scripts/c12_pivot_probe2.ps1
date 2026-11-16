# C12.4：用 Excel 打开带真 pivotTable 部件的文件，确认是可编辑透视表（不是值化网格）。
# 输出：sheet / pivot 数量 / 名称 / 行字段 / 列字段 / 数据字段 / 表范围 / 刷新后前几格的值
$ErrorActionPreference = 'Stop'
$file = $args[0]
if ([string]::IsNullOrWhiteSpace($file)) { $file = 'work/probe/c12/pivot-real.xlsx' }
$file = (Resolve-Path $file).Path.Replace('/', '\')
$appName = if ($args.Count -gt 1) { $args[1] } else { 'Excel.Application' }

function Retry([scriptblock]$sb, [int]$n = 40) {
    for ($i = 0; $i -lt $n; $i++) {
        try { return & $sb } catch {
            if ($_.Exception.Message -notmatch 'rejected by callee|0x80010001|0x8001010A') { throw }
            Start-Sleep -Milliseconds 500
        }
    }
    throw 'retry exhausted'
}

$xl = New-Object -ComObject $appName
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = Retry { $xl.Workbooks.Open($file, 0, $false) }

"app=" + $appName + " version=" + $xl.Version
"sheets=" + $wb.Worksheets.Count
$total = 0
foreach ($ws in $wb.Worksheets) {
    $pts = Retry { $ws.PivotTables() }
    $count = @($pts).Count
    "{0}: PivotTables={1}" -f $ws.Name, $count
    foreach ($pt in $pts) {
        $total++
        $rowFields = @(); foreach ($f in (Retry { $pt.RowFields() })) { $rowFields += $f.Name }
        $colFields = @(); foreach ($f in (Retry { $pt.ColumnFields() })) { $colFields += $f.Name }
        $dataFields = @(); foreach ($f in (Retry { $pt.DataFields() })) { $dataFields += $f.Name }
        "  name=$($pt.Name) rows=[$($rowFields -join ',')] cols=[$($colFields -join ',')] data=[$($dataFields -join ',')]"
        "  range=$((Retry { $pt.TableRange1.Address($false,$false) })) src=$((Retry { $pt.PivotCache().SourceData }))"
        try { Retry { $pt.RefreshTable() } | Out-Null } catch { "  refresh failed: $($_.Exception.Message)" }
        $rng = Retry { $pt.TableRange1 }
        $nc = $rng.Columns.Count
        "  values:"
        for ($r = 1; $r -le [Math]::Min(6, $rng.Rows.Count); $r++) {
            $line = @()
            for ($c = 1; $c -le $nc; $c++) { $line += [string](Retry { $rng.Cells($r, $c).Text }) }
            "    " + ($line -join '|')
        }
    }
}
"total=" + $total
$wb.Close($false)
$xl.Quit()
