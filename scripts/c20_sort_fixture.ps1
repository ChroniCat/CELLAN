$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c20\sort.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1); $ws.Name = "姓名"
    $ws.Range("A1").Value2 = "姓名"
    $names = @("张三", "李四", "王五", "赵六", "陈七")
    for ($i = 0; $i -lt $names.Count; $i++) { $ws.Range("A" + ($i + 2)).Value2 = [string]$names[$i] }
    # Excel 默认中文排序（拼音）
    $rng = $ws.Range("A2:A6")
    $rng.Sort($ws.Range("A2"), 1)
    $sorted = @()
    for ($i = 2; $i -le 6; $i++) { $sorted += [string]$ws.Range("A$i").Value2 }
    "EXCEL_SORTED=" + ($sorted -join ",")

    "EXCEL_SORTED_DONE"
    $wb.SaveAs($out)
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
