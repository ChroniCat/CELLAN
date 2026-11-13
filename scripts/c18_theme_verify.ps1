$ErrorActionPreference = "Stop"
$src = Join-Path $env:TEMP "sc_c18_theme_edit.xlsx"
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Open($src)
    $ws = $wb.Worksheets.Item(1)
    $c = $ws.Range("A1")
    "FONT_THEME={0} FONT_TINT={1} FONT_COLOR={2:X6}" -f $c.Font.ThemeColor, $c.Font.TintAndShade, ($c.Font.Color -band 0xFFFFFF)
    "FILL_THEME={0} FILL_TINT={1} FILL_COLOR={2:X6}" -f $c.Interior.ThemeColor, $c.Interior.TintAndShade, ($c.Interior.Color -band 0xFFFFFF)
    "BOLD={0}" -f $c.Font.Bold
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
