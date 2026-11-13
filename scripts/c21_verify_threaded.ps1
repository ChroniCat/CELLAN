$ErrorActionPreference = "Stop"
$src = Join-Path $env:TEMP "sc_c21_threaded.xlsx"
function Probe($app, $label) {
    $wb = $app.Workbooks.Open($src)
    try {
        $ws = $wb.Worksheets.Item(1)
        $txt = ""
        try { $txt = $ws.Range("A1").CommentThreaded.Text } catch { $txt = "<无 CommentThreaded>" }
        "{0}: THREAD='{1}' B1='{2}' B1FMT='{3}'" -f $label, ($txt -replace "`r?`n", " "), $ws.Range("B1").Text, $ws.Range("B1").NumberFormat
    } finally { $wb.Close($false) }
}
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false; $xl.DisplayAlerts = $false
try { Probe $xl "EXCEL" } finally {
    $xl.Quit(); [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
try {
    $ket = New-Object -ComObject KET.Application
    $ket.Visible = $false; $ket.DisplayAlerts = $false
    try { Probe $ket "WPS" } finally {
        $ket.Quit(); [System.Runtime.InteropServices.Marshal]::ReleaseComObject($ket) | Out-Null
    }
} catch { "WPS: 不可用 $($_.Exception.Message)" }
