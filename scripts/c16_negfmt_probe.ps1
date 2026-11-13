$ErrorActionPreference = "Stop"
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $fmts = @('0.00', '(#,##0.00)', '[Red]-#,##0.00', '#,##0.00"元"', '0.00;[Red]-0.00', '#,##0.00;(#,##0.00)')
    $i = 1
    foreach ($f in $fmts) {
        $ws.Cells.Item($i, 1).Value2 = -1234.5
        $ws.Cells.Item($i, 1).NumberFormat = $f
        "fmt=[$f] text=[$($ws.Cells.Item($i,1).Text)]"
        $i++
    }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
