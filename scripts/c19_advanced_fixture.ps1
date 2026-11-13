$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c19\advanced.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    # 雷达
    $ws = $wb.Worksheets.Item(1); $ws.Name = "雷达"
    $ws.Range("A1").Value2 = "能力"; $ws.Range("B1").Value2 = "得分"
    $labels = @("速度", "力量", "技巧", "耐力", "意识")
    $vals = @(8, 6, 9, 7, 5)
    for ($i = 0; $i -lt 5; $i++) {
        $r = $i + 2
        $ws.Range("A$r").Value2 = [string]$labels[$i]
        $ws.Range("B$r").Value2 = [double]$vals[$i]
    }
    $sh = $ws.Shapes.AddChart2(251, 81, 20, 20, 360, 240)   # xlRadar
    $ch = $sh.Chart; $ch.SetSourceData($ws.Range("A1:B6"))

    # 瀑布（chartex）
    $ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws); $ws2.Name = "瀑布"
    $ws2.Range("A1").Value2 = "项"; $ws2.Range("B1").Value2 = "金额"
    $items = @("收入", "成本", "税费", "净利")
    $amt = @(100, -40, -10, 50)
    for ($i = 0; $i -lt 4; $i++) {
        $r = $i + 2
        $ws2.Range("A$r").Value2 = [string]$items[$i]
        $ws2.Range("B$r").Value2 = [double]$amt[$i]
    }
    $ws2.Range("A1:B5").Select()
    $sh2 = $ws2.Shapes.AddChart2(251, 119, 20, 20, 360, 240)  # xlWaterfall
    $ch2 = $sh2.Chart

    # 直方图（chartex）
    $ws3 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws2); $ws3.Name = "直方"
    $ws3.Range("A1").Value2 = "分数"
    $scores = @(55, 62, 67, 71, 73, 78, 82, 85, 88, 91, 95, 98)
    for ($i = 0; $i -lt $scores.Count; $i++) { $ws3.Range("A" + ($i + 2)).Value2 = [double]$scores[$i] }
    $ws3.Range("A1:A13").Select()
    $sh3 = $ws3.Shapes.AddChart2(251, 118, 20, 20, 360, 240)  # xlHistogram
    $ch3 = $sh3.Chart

    $wb.SaveAs($out)
    "RADAR=" + $ch.ChartType + " WF=" + $ch2.ChartType + " HIST=" + $ch3.ChartType
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
