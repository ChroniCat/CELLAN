$ErrorActionPreference = "Stop"
$src = Join-Path $env:TEMP "sc_c18_pattern_rt2.xlsx"
function Probe($app, $label) {
    $wb = $app.Workbooks.Open($src)
    try {
        $ws = $wb.Worksheets.Item(1)
        $pat = $ws.Range("A1").Interior.Pattern
        $diag = $ws.Range("B1").Borders.Item(5).LineStyle
        $diagc = $ws.Range("B1").Borders.Item(5).Color -band 0xFFFFFF
        $fmt = $ws.Range("C1").NumberFormat
        "{0}: PATTERN={1} DIAG_DOWN={2} DIAG_COLOR={3:X6} FMT={4}" -f $label, $pat, $diag, $diagc, $fmt
    } finally {
        $wb.Close($false)
    }
}
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try { Probe $xl "EXCEL" } finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
try {
    $ket = New-Object -ComObject KET.Application
    $ket.Visible = $false
    $ket.DisplayAlerts = $false
    try { Probe $ket "WPS" } finally {
        $ket.Quit()
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($ket) | Out-Null
    }
} catch {
    "WPS: 不可用 $($_.Exception.Message)"
}
