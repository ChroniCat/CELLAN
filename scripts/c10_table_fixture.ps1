$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c10\table.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "数据"
    $ws.Range("A1").Value2 = "Amount"
    $ws.Range("B1").Value2 = "Tax"
    $ws.Range("C1").Value2 = "Total"
    for ($r = 2; $r -le 4; $r++) {
        $ws.Cells.Item($r, 1).Value2 = $r - 1
        $ws.Cells.Item($r, 2).Value2 = 2
        $ws.Cells.Item($r, 3).Value2 = ($r - 1) * 2
    }
    $null = $ws.ListObjects.Add(1, $ws.Range("A1:C4"), $null, 1)
    $ws.ListObjects.Item(1).Name = "Sales"
    $ws.Range("E1").Formula = "=SUM(Sales[Amount])"
    $ws.Range("E2").Formula = "=Sales[[#This Row],[Amount]]*2"
    $wb.SaveAs($out)
    "TABLES=" + $ws.ListObjects.Count + " E1=" + $ws.Range("E1").Value2
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
