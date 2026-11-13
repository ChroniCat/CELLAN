$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c19\combo.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "销售"
    $ws.Range("A1").Value2 = "月份"
    $ws.Range("B1").Value2 = "销量"
    $ws.Range("C1").Value2 = "增长率"
    $months = @("1月", "2月", "3月", "4月")
    $sales = @(120, 150, 90, 200)
    $rate = @(0.12, 0.25, -0.4, 1.22)
    for ($i = 0; $i -lt 4; $i++) {
        $r = $i + 2
        $ws.Range("A$r").Value2 = [string]$months[$i]
        $ws.Range("B$r").Value2 = [double]$sales[$i]
        $ws.Range("C$r").Value2 = [double]$rate[$i]
        $ws.Range("C$r").NumberFormat = "0%"
    }
    $sh = $ws.Shapes.AddChart2(251, 51, 20, 20, 420, 240)   # xlColumnClustered
    $ch = $sh.Chart
    $ch.SetSourceData($ws.Range("A1:C5"))
    $ch.SeriesCollection(2).ChartType = 4               # xlLine
    $ch.SeriesCollection(2).AxisGroup = 2               # xlSecondary
    $ch.HasTitle = $true
    $ch.ChartTitle.Text = "销量与增长率"
    $wb.SaveAs($out)
    "SERIES=" + $ch.SeriesCollection().Count + " TYPE2=" + $ch.SeriesCollection(2).ChartType + " AXIS2=" + $ch.SeriesCollection(2).AxisGroup
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
