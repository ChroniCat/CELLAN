# 透视表 Excel 探针：每个文件单独起实例、只读打开、等 1.5s（避免 COM 抖动误判）
param([string]$File)
$ErrorActionPreference = 'Stop'
$path = $File.Replace('/', [IO.Path]::DirectorySeparatorChar)
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Open($path, 0, $true)
    Start-Sleep -Milliseconds 1500
    $ws = $wb.Worksheets.Item(1)
    $c = $ws.PivotTables.Count
    $out = [IO.Path]::GetFileName($path) + " sheet=" + $ws.Name + " pivots=" + $c
    if ($c -gt 0) {
        $pt = $ws.PivotTables(1)
        $out += " name=" + $pt.Name + " rows=" + $pt.RowFields.Count + " cols=" + $pt.ColumnFields.Count + " data=" + $pt.DataFields.Count
        # 第一行数据值，确认缓存能取数
        try {
            $rng = $pt.TableRange1
            $out += " range=" + $rng.Address($false, $false)
        } catch { $out += " range=<err>" }
    }
    $out
    $wb.Close($false)
} catch {
    "FAIL " + [IO.Path]::GetFileName($path) + " " + $_.Exception.Message
} finally {
    $xl.Quit()
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
