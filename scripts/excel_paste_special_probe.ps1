$ErrorActionPreference = "Stop"
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1").Value2 = 3
    $ws.Range("B1").Value2 = 10
    $ws.Range("C3").Formula = "=A1+`$B`$1"
    $ws.Range("C3").Copy()
    $ws.Range("B2").PasteSpecial(-4123)   # xlPasteFormulas
    $f1 = $ws.Range("B2").Formula
    $ws.Range("C3").Copy()
    $ws.Range("D4").PasteSpecial(-4123)
    $f2 = $ws.Range("D4").Formula
    "C3=" + $ws.Range("C3").Formula
    "B2=" + $f1
    "D4=" + $f2
    # 转置：A1:B1 两个公式，复制后转置粘到 D11
    $ws.Range("A1").Formula = "=A1"
    $ws.Range("B1").Formula = "=B1"
    $ws.Range("A1:B1").Copy()
    $ws.Range("D11").PasteSpecial(-4123, -4142, $false, $true)  # formulas, none, false, transpose
    "D11=" + $ws.Range("D11").Formula
    "D12=" + $ws.Range("D12").Formula
} finally {
    $wb.Close($false)
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
