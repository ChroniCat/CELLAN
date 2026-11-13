$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c18\theme.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "主题"
    $ws.Range("A1").Value2 = "主题字体与填充"
    $ws.Range("A1").Font.ThemeColor = 5          # xlThemeColorAccent1
    $ws.Range("A1").Font.TintAndShade = 0.4
    $ws.Range("A1").Interior.ThemeColor = 6      # xlThemeColorAccent2
    $ws.Range("A1").Interior.TintAndShade = -0.25
    $ws.Range("B1").Value2 = 42
    $ws.Range("B1").Font.Bold = $true
    $ws.Columns.Item(1).ColumnWidth = 22
    $wb.SaveAs($out)
    $c = $ws.Range("A1")
    "THEME_FONT={0:X6} THEME_FILL={1:X6}" -f ($c.Font.Color -band 0xFFFFFF), ($c.Interior.Color -band 0xFFFFFF)
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
