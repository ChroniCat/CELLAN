$ErrorActionPreference = "Stop"
$path = "F:\finance-native-sheets-ready\work\goal-logs\gui\ui20-saved.xlsx"
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Open($path, 0, $true)
    $ws = $wb.Worksheets.Item(1)
    "shapes=" + $ws.Shapes.Count
    foreach ($sh in $ws.Shapes) {
        "name=" + $sh.Name + " type=" + $sh.Type + " tl=" + $sh.TopLeftCell.Address($false,$false) + " br=" + $sh.BottomRightCell.Address($false,$false) + " w=" + [math]::Round($sh.Width,1) + " h=" + [math]::Round($sh.Height,1)
    }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
