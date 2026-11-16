# 用 Excel 自己造三种「多字段」透视表当结构黄金样本：
#   A: 2 个行字段 + 1 个值，无列字段
#   B: 1 个行字段 + 1 个列字段 + 2 个值字段
#   C: 2 个行字段 + 1 个列字段 + 1 个值字段
$ErrorActionPreference = 'Stop'
$dir = 'F:\finance-native-sheets-ready\work\probe\c12\gold'
New-Item -ItemType Directory -Force -Path $dir | Out-Null

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false; $xl.DisplayAlerts = $false

function New-Book {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = 'Data'
    $ws.Cells.Item(1,1) = '地区'; $ws.Cells.Item(1,2) = '产品'; $ws.Cells.Item(1,3) = '金额'; $ws.Cells.Item(1,4) = '数量'
    $rows = @(
      @('华东','A',10,2), @('华北','B',20,3), @('华东','A',30,4),
      @('华北','B',40,5), @('华东','B',50,6), @('华北','A',60,7)
    )
    for ($i = 0; $i -lt $rows.Count; $i++) {
        $r = $i + 2
        $ws.Cells.Item($r,1) = $rows[$i][0]
        $ws.Cells.Item($r,2) = $rows[$i][1]
        $ws.Cells.Item($r,3) = [double]$rows[$i][2]
        $ws.Cells.Item($r,4) = [double]$rows[$i][3]
    }
    return @($wb, $ws)
}

# A: 2 行字段 + 1 值
$p = New-Book; $wb = $p[0]; $ws = $p[1]
$pc = $wb.PivotCaches().Create(1, $ws.Range('A1:D7'))
$pt = $pc.CreatePivotTable($ws.Range('F3'), 'PTA')
$pt.PivotFields('地区').Orientation = 1
$pt.PivotFields('产品').Orientation = 1
$pt.AddDataField($pt.PivotFields('金额'), '求和项:金额', -4157) | Out-Null
$pt.RowAxisLayout(1) | Out-Null
$wb.SaveAs((Join-Path $dir 'A-2rows.xlsx'), 51); $wb.Close($false)

# B: 1 行 + 1 列 + 2 值
$p = New-Book; $wb = $p[0]; $ws = $p[1]
$pc = $wb.PivotCaches().Create(1, $ws.Range('A1:D7'))
$pt = $pc.CreatePivotTable($ws.Range('F3'), 'PTB')
$pt.PivotFields('地区').Orientation = 1
$pt.PivotFields('产品').Orientation = 2
$pt.AddDataField($pt.PivotFields('金额'), '求和项:金额', -4157) | Out-Null
$pt.AddDataField($pt.PivotFields('数量'), '求和项:数量', -4157) | Out-Null
$wb.SaveAs((Join-Path $dir 'B-2values.xlsx'), 51); $wb.Close($false)

# C: 2 行 + 1 列 + 1 值
$p = New-Book; $wb = $p[0]; $ws = $p[1]
$pc = $wb.PivotCaches().Create(1, $ws.Range('A1:D7'))
$pt = $pc.CreatePivotTable($ws.Range('F3'), 'PTC')
$pt.PivotFields('地区').Orientation = 1
$pt.PivotFields('产品').Orientation = 1
$pt.PivotFields('数量').Orientation = 2
$pt.AddDataField($pt.PivotFields('金额'), '求和项:金额', -4157) | Out-Null
$pt.RowAxisLayout(1) | Out-Null
$wb.SaveAs((Join-Path $dir 'C-2rows1col.xlsx'), 51); $wb.Close($false)

$xl.Quit()
"gold samples written to $dir"
