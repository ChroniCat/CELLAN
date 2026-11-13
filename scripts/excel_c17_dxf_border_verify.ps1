$ErrorActionPreference = "Stop"
$src = "F:\finance-native-sheets-ready\work\goal-logs\gui\c17-borders-saved.xlsx"
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Open($src)
    $ws = $wb.Worksheets.Item(1)
    $fc = $ws.Range("A1").FormatConditions
    "rules=$($fc.Count)"
    for ($i = 1; $i -le $fc.Count; $i++) {
        $c = $fc.Item($i)
        "rule$i type=$($c.Type) priority=$($c.Priority)"
        if ($c.Type -eq 2) {
            foreach ($e in 7, 10, 8, 9) {
                $bd = $c.Borders.Item($e)
                "  edge$e style=$($bd.LineStyle) weight=$($bd.Weight) color=$($bd.Color)"
            }
        }
    }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
