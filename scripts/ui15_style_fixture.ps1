$ErrorActionPreference = "Stop"
$dir = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\ui15"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$out = Join-Path $dir "style.xlsx"
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1); $ws.Name = "格式"
    $ws.Range("A1").Value2 = "财务报表"
    $ws.Range("A1").Font.Underline = -4119      # xlUnderlineStyleDouble
    $ws.Range("A1").Orientation = 255           # 竖排
    $ws.Range("B2").Value2 = "单下划线"
    $ws.Range("B2").Font.Underline = 2          # xlUnderlineStyleSingle
    $ws.Range("C3").Value2 = "旋转90"
    $ws.Range("C3").Orientation = 90
    "FIXTURE A1_UL=" + $ws.Range("A1").Font.Underline + " A1_ORI=" + $ws.Range("A1").Orientation + " B2_UL=" + $ws.Range("B2").Font.Underline
    $wb.SaveAs($out)
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
