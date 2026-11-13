$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c16\objects.xlsx"
$png = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c16\logo.png"
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "对象"
    $ws.Range("A1").Value2 = "月份"
    $ws.Range("B1").Value2 = "销量"
    $ws.Range("A2").Value2 = "一月"; $ws.Range("B2").Value2 = 12
    $ws.Range("A3").Value2 = "二月"; $ws.Range("B3").Value2 = 18
    $ws.Range("A4").Value2 = "三月"; $ws.Range("B4").Value2 = 9
    $ws.Range("A5").Value2 = "四月"; $ws.Range("B5").Value2 = 22

    $chart = $ws.Shapes.AddChart2(201, 51, 120, 10, 360, 200)
    $chart.Chart.SetSourceData($ws.Range("A1:B5"))
    $chart.Chart.HasTitle = $true
    $chart.Chart.ChartTitle.Text = "季度销量"

    $pic = $ws.Shapes.AddPicture($png, 0, 1, 20, 360, 96, 64)
    $pic.Name = "Logo"

    $tb = $ws.Shapes.AddTextbox(1, 20, 470, 260, 60)
    $tb.TextFrame.Characters().Text = "打印文本框：图表与图片要和预览一致"
    $tb.TextFrame.Characters().Font.Size = 11

    $ws.PageSetup.PrintArea = "A1:H40"
    $ws.PageSetup.Orientation = 1
    $wb.SaveAs($out)
    $wb.Close($false)
    "SAVED=$out"
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
