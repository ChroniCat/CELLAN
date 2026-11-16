# QA-01 特性矩阵探针：在 Excel / WPS 里逐项读取样式、条件格式、数据验证、表格、命名区域、冻结、隐藏。
# 用法：powershell -File qa01_features_probe.ps1 <file.xlsx> [Excel.Application|KET.Application]
$ErrorActionPreference = 'Stop'
$file = (Resolve-Path $args[0]).Path.Replace('/', '\')
$prog = if ($args.Count -ge 2) { $args[1] } else { 'Excel.Application' }

$app = New-Object -ComObject $prog
$app.Visible = $false
try { $app.DisplayAlerts = $false } catch {}
$wb = $app.Workbooks.Open($file, 0, $true)
try {
    "product=" + $app.Name + " " + $app.Version
    $ws = $wb.Worksheets.Item(1)
    "sheet=" + $ws.Name

    # 样式
    "a1.bold=" + $ws.Range('A1').Font.Bold
    "a1.fill=" + $ws.Range('A1').Interior.Color
    "b2.numfmt=" + $ws.Range('B2').NumberFormat
    "b2.border_bottom=" + $ws.Range('B2').Borders(9).LineStyle + " color=" + $ws.Range('B2').Borders(9).Color

    # 条件格式
    $fc = $ws.Range('B2:B5').FormatConditions
    "cf.count=" + $fc.Count
    $types = @()
    for ($i = 1; $i -le $fc.Count; $i++) { $types += [string]$fc.Item($i).Type }
    "cf.types=" + ($types -join ',')

    # 数据验证
    "dv.c2.type=" + $ws.Range('C2').Validation.Type + " formula=" + $ws.Range('C2').Validation.Formula1
    "dv.d2.type=" + $ws.Range('D2').Validation.Type + " op=" + $ws.Range('D2').Validation.Operator + " f1=" + $ws.Range('D2').Validation.Formula1 + " f2=" + $ws.Range('D2').Validation.Formula2

    # 表格对象
    "listobjects.count=" + $ws.ListObjects.Count
    if ($ws.ListObjects.Count -ge 1) {
        $lo = $ws.ListObjects.Item(1)
        "listobjects.name=" + $lo.Name + " range=" + $lo.Range.Address()
    }

    # 命名区域
    "names.count=" + $wb.Names.Count
    foreach ($n in $wb.Names) { "name=" + $n.Name + " -> " + $n.RefersTo }

    # 冻结窗格（读活动窗口的分割行/列）
    # 不要写 SplitRow=SplitRow：WPS 会因此把列分割清掉，读到假的 0
    "freeze.splitrow=" + $app.ActiveWindow.SplitRow
    "freeze.splitcol=" + $app.ActiveWindow.SplitColumn

    # 隐藏行/列
    "hidden.row7=" + $ws.Rows.Item(7).Hidden
    "hidden.colE=" + $ws.Columns.Item(5).Hidden
} finally {
    $wb.Close($false)
    $app.Quit()
}
