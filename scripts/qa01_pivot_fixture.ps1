$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\qa01\pivot.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1); $ws.Name = "数据"
    foreach ($pair in @(@(1,1,"地区"),@(1,2,"城市"),@(1,3,"销售额"))) { $ws.Cells.Item($pair[0], $pair[1]).Value2 = $pair[2] }
    $rows = @(@("华东","上海",100),@("华东","上海",50),@("华北","北京",20),@("华南","广州",70))
    for ($i = 0; $i -lt $rows.Count; $i++) {
        $ws.Cells.Item($i + 2, 1).Value2 = [string]$rows[$i][0]
        $ws.Cells.Item($i + 2, 2).Value2 = [string]$rows[$i][1]
        $ws.Cells.Item($i + 2, 3).Value2 = [double]$rows[$i][2]
    }
    $ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws); $ws2.Name = "透视"
    $pc = $wb.PivotCaches().Create(1, $ws.Range("A1:C5"))
    $pt = $pc.CreatePivotTable($ws2.Range("A3"), "P1")
    $pt.RowAxisLayout(1) | Out-Null
    $pt.PivotFields("地区").Orientation = 1
    $pt.PivotFields("城市").Orientation = 1
    $pt.AddDataField($pt.PivotFields("销售额"), "销售额 求和", -4157) | Out-Null
    $pt.RefreshTable()
    "FIXTURE pivot=" + $ws2.PivotTables().Count + " fields=" + $pt.PivotFields().Count
    $wb.SaveAs($out)
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
