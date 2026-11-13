$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c19\axes.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "进度"
    $ws.Range("A1").Value2 = "阶段"
    $ws.Range("B1").Value2 = "完成率"
    $names = @("设计", "开发", "测试", "上线")
    $vals = @(0.15, 0.4, 0.65, 0.9)
    for ($i = 0; $i -lt 4; $i++) {
        $r = $i + 2
        $ws.Range("A$r").Value2 = [string]$names[$i]
        $ws.Range("B$r").Value2 = [double]$vals[$i]
        $ws.Range("B$r").NumberFormat = "0%"
    }
    $sh = $ws.Shapes.AddChart2(251, 51, 20, 20, 380, 220)
    $ch = $sh.Chart
    $ch.SetSourceData($ws.Range("A1:B5"))
    $ax = $ch.Axes(2, 1)                       # xlValue, primary
    $ax.TickLabels.NumberFormat = "0%"
    $ax.HasMajorGridlines = $false

    $ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws)
    $ws2.Name = "散点"
    $ws2.Range("A1").Value2 = "X"
    $ws2.Range("B1").Value2 = "Y"
    $xs = @(1.0, 2.0, 10.0, 50.0, 100.0)
    $ys = @(2.0, 4.5, 5.0, 9.0, 12.0)
    for ($i = 0; $i -lt 5; $i++) {
        $r = $i + 2
        $ws2.Range("A$r").Value2 = [double]$xs[$i]
        $ws2.Range("B$r").Value2 = [double]$ys[$i]
    }
    $sh2 = $ws2.Shapes.AddChart2(240, 73, 20, 20, 380, 220)   # xlXYScatter (no markers? 73 = xlXYScatter)
    $ch2 = $sh2.Chart
    $ch2.SetSourceData($ws2.Range("A1:B6"))

    $ws3 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws2)
    $ws3.Name = "异常"
    $ws3.Range("A1").Value2 = "项"
    $ws3.Range("B1").Value2 = "值"
    $labels = @("甲", "乙", "丙", "丁")
    for ($i = 0; $i -lt 4; $i++) { $ws3.Range("A" + ($i + 2)).Value2 = [string]$labels[$i] }
    $ws3.Range("B2").Value2 = [double]5
    $ws3.Range("B3").Value2 = [double]-3
    # B4 留空
    $ws3.Range("B5").Formula = "=NA()"
    $sh3 = $ws3.Shapes.AddChart2(251, 51, 20, 20, 380, 220)
    $ch3 = $sh3.Chart
    $ch3.SetSourceData($ws3.Range("A1:B5"))

    $wb.SaveAs($out)
    "PCT_FMT=" + $ch.Axes(2, 1).TickLabels.NumberFormat + " SCATTER=" + $ch2.ChartType + " SERIES=" + $ch2.SeriesCollection().Count + " BAD=" + $ws3.Range("B3").Value2 + "/" + $ws3.Range("B4").Text + "/" + $ws3.Range("B5").Text
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
