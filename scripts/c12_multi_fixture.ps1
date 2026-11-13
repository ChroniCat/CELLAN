$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c12\multi.xlsx"
New-Item -ItemType Directory -Force -Path (Split-Path $out) | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1); $ws.Name = "数据"
    $head = @("地区","城市","产品","销售额","数量","状态")
    for ($c = 0; $c -lt $head.Count; $c++) { $ws.Cells.Item(1, $c + 1).Value2 = [string]$head[$c] }
    $rows = @(
        @("华东","上海","甲",100,1,"有效"),
        @("华东","上海","乙",50,2,"有效"),
        @("华东","杭州","甲",30,3,"有效"),
        @("华北","北京","甲",20,4,"有效"),
        @("华北","北京","乙",$null,5,"有效"),
        @("华北","天津","甲","x",6,"无效")
    )
    for ($i = 0; $i -lt $rows.Count; $i++) {
        for ($c = 0; $c -lt 6; $c++) {
            $v = $rows[$i][$c]
            if ($null -ne $v) {
                if ($v -is [int]) { $ws.Cells.Item($i + 2, $c + 1).Value2 = [double]$v }
                else { $ws.Cells.Item($i + 2, $c + 1).Value2 = [string]$v }
            }
        }
    }
    $ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws); $ws2.Name = "透视"
    $pc = $wb.PivotCaches().Create(1, $ws.Range("A1:F7"))
    $pt = $pc.CreatePivotTable($ws2.Range("A3"), "P1")
    $pt.RowAxisLayout(1) | Out-Null     # xlTabularRow
    $pt.PivotFields("地区").Orientation = 1
    $pt.PivotFields("城市").Orientation = 1
    $pt.PivotFields("产品").Orientation = 2
    $pt.AddDataField($pt.PivotFields("销售额"), "销售额 求和", -4157) | Out-Null
    $pt.AddDataField($pt.PivotFields("数量"), "数量 计数", -4112) | Out-Null
    $pf = $pt.PivotFields("状态")
    $pf.Orientation = 3
    $pf.CurrentPage = "有效"
    $pt.RefreshTable()
    $used = $ws2.UsedRange
    "PIVOT RANGE=" + $used.Address()
    for ($r = 1; $r -le [Math]::Min($used.Rows.Count, 20); $r++) {
        $line = @()
        for ($c = 1; $c -le [Math]::Min($used.Columns.Count, 8); $c++) {
            $line += [string]$ws2.Cells.Item($r, $c).Text
        }
        "ROW $r | " + ($line -join " | ")
    }
    $wb.SaveAs($out)
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
