$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c21\macro.xlsm"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1").Value2 = "宏与自定义部件"
    $ws.Range("B1").Value2 = 7
    try { $wb.CustomXMLParts.Add("<root xmlns='urn:c21'><v>1</v></root>") | Out-Null; $cx = "CUSTOMXML_OK" } catch { $cx = "CUSTOMXML_FAIL" }
    $wb.SaveAs($out, 52)   # xlOpenXMLWorkbookMacroEnabled
    "SAVED={0} CUSTOMXML={1} HASVB={2}" -f (Test-Path $out), $cx, $wb.HasVBProject
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
