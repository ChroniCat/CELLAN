$ErrorActionPreference = "Stop"
$out = "F:\finance-native-sheets-ready\crates\core\tests\fixtures\c17\priority.xlsx"
$dir = Split-Path $out -Parent
New-Item -ItemType Directory -Force -Path $dir | Out-Null
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "CF"
    $ws.Range("A1").Value2 = 1000
    $ws.Range("A2").Value2 = 1100
    $ws.Range("A3").Value2 = 100000
    $ws.Range("B1:B3").Value2 = $ws.Range("A1:A3").Value2
    $ws.Range("C1:C3").Value2 = 5
    $ws.Range("D1:D3").Value2 = 7

    # A 列：三点色阶，中点 = 50%（percent）
    $a = $ws.Range("A1:A3").FormatConditions.AddColorScale(3)
    $a.ColorScaleCriteria.Item(1).Type = 1
    $a.ColorScaleCriteria.Item(1).FormatColor.Color = 255
    $a.ColorScaleCriteria.Item(2).Type = 3
    $a.ColorScaleCriteria.Item(2).Value = 50
    $a.ColorScaleCriteria.Item(2).FormatColor.Color = 65535
    $a.ColorScaleCriteria.Item(3).Type = 2
    $a.ColorScaleCriteria.Item(3).FormatColor.Color = 65280

    # B 列：三点色阶，中点 = 50 百分位（percentile）
    $b = $ws.Range("B1:B3").FormatConditions.AddColorScale(3)
    $b.ColorScaleCriteria.Item(1).Type = 1
    $b.ColorScaleCriteria.Item(1).FormatColor.Color = 255
    $b.ColorScaleCriteria.Item(2).Type = 5
    $b.ColorScaleCriteria.Item(2).Value = 50
    $b.ColorScaleCriteria.Item(2).FormatColor.Color = 65535
    $b.ColorScaleCriteria.Item(3).Type = 2
    $b.ColorScaleCriteria.Item(3).FormatColor.Color = 65280

    # C 列：两条同条件规则，priority 2 在前、priority 1 在后（XML 顺序与 priority 逆序）
    $r2 = $ws.Range("C1:C3").FormatConditions.Add(2, 0, "=C1>0")
    $r2.Interior.Color = 255
    $r2.StopIfTrue = $false
    $r2.Priority = 5
    $r1 = $ws.Range("C1:C3").FormatConditions.Add(2, 0, "=C1>0")
    $r1.Interior.Color = 16711680
    $r1.StopIfTrue = $false
    $r1.Priority = 3

    # D 列：第一条 stopIfTrue 且覆盖三个格；第二条本应上色但应被停
    $d1 = $ws.Range("D1:D3").FormatConditions.Add(2, 0, "=D1>0")
    $d1.Interior.Color = 255
    $d1.StopIfTrue = $true
    $d1.Priority = 1
    $d2 = $ws.Range("D1:D3").FormatConditions.Add(2, 0, "=D1>0")
    $d2.Interior.Color = 16711680
    $d2.StopIfTrue = $false
    $d2.Priority = 2

    $wb.SaveAs($out)
    # 导出同区域图片，供像素对照
    $ws.Range("A1:D3").CopyPicture(1, 2) | Out-Null
    Start-Sleep -Milliseconds 800
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    $img = [System.Windows.Forms.Clipboard]::GetImage()
    if ($img -ne $null) {
        $png = "F:\finance-native-sheets-ready\work\goal-logs\gui\c17-excel.png"
        $img.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
        "PICTURE=$png $($img.Width)x$($img.Height)"
    }
    "SAVED=$out"
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
