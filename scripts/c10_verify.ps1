$ErrorActionPreference = "Stop"
function Probe($file, $label) {
    $xl = New-Object -ComObject Excel.Application
    $xl.Visible = $false; $xl.DisplayAlerts = $false
    try {
        $wb = $xl.Workbooks.Open($file)
        $ws = $wb.Worksheets.Item(1)
        $n = $ws.ListObjects.Count
        $col = if ($n -gt 0) { $ws.ListObjects.Item(1).ListColumns.Item(1).Name } else { "<无表格>" }
        "{0}: TABLES={1} COL1='{2}' E1={3} E2={4} E1F='{5}'" -f $label, $n, $col, $ws.Range("E1").Value2, $ws.Range("E2").Value2, $ws.Range("E1").Formula
        $wb.Close($false)
    } finally { $xl.Quit(); [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null }
}
Probe (Join-Path $env:TEMP "sc_c10_renamed.xlsx") "RENAMED"
Probe (Join-Path $env:TEMP "sc_c10_converted.xlsx") "CONVERTED"
