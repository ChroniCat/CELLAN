# 用本机 Excel 的真实行为核对数据验证列表来源语义（新建隐藏实例，不碰用户文档）
$ErrorActionPreference = 'Stop'
$out = 'C:\Users\Administrator\AppData\Local\hermes\cache\scratch\dvprobe'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = '主表'
    $ws.Range('A1').Value2 = 10
    $ws.Range('A2').Value2 = 20
    $ws.Range('A3').Value2 = 30
    $ws.Range('A4').Value2 = 40

    # 1) 相对来源 =$A1:$A2 覆盖 C2:C3
    $rng = $ws.Range('C2:C3')
    $rng.Validation.Delete()
    $rng.Validation.Add(3, 1, 1, '=$A1:$A2')
    $c2f = $ws.Range('C2').Validation.Formula1
    $c3f = $ws.Range('C3').Validation.Formula1
    $ws.Range('C3').Value2 = 30
    $c3ok = $ws.Range('C3').Validation.Value
    $ws.Range('C3').ClearContents()
    $ws.Range('C3').Value2 = 10
    $c3bad = $ws.Range('C3').Validation.Value
    $ws.Range('C3').ClearContents()
    Write-Output "REL C2=$c2f C3=$c3f val30=$c3ok val10=$c3bad"

    # 2) 动态来源 OFFSET（名称 + 直接公式）
    $ws.Range('E1').Validation.Delete()
    $ws.Range('E1').Validation.Add(3, 1, 1, '=OFFSET($A$1,0,0,COUNTA($A:$A),1)')
    $dynf = $ws.Range('E1').Validation.Formula1
    $ws.Range('E1').Value2 = 30
    $dynok = $ws.Range('E1').Validation.Value
    $ws.Range('E1').ClearContents()
    $ws.Range('E1').Value2 = 99
    $dynbad = $ws.Range('E1').Validation.Value
    $ws.Range('E1').ClearContents()
    Write-Output "DYN formula=$dynf val30=$dynok val99=$dynbad"

    # 3) 名称指向 OFFSET
    $wb.Names.Add('动态表', '=OFFSET(主表!$A$1,0,0,COUNTA(主表!$A:$A),1)') | Out-Null
    $ws.Range('G1').Validation.Delete()
    $ws.Range('G1').Validation.Add(3, 1, 1, '=动态表')
    $ws.Range('G1').Value2 = 20
    $nameok = $ws.Range('G1').Validation.Value
    $ws.Range('G1').ClearContents()
    Write-Output "NAME formula=$($ws.Range('G1').Validation.Formula1) val20=$nameok"

    # 4) 括号工作表名
    $ws2 = $wb.Worksheets.Add()
    $ws2.Name = '预算(2026)'
    $ws2.Range('A1').Value2 = '甲'
    $ws2.Range('A2').Value2 = '乙'
    $ws.Range('I1').Validation.Delete()
    $ws.Range('I1').Validation.Add(3, 1, 1, "='预算(2026)'!`$A`$1:`$A`$2")
    $parf = $ws.Range('I1').Validation.Formula1
    $ws.Range('I1').Value2 = '乙'
    $parok = $ws.Range('I1').Validation.Value
    $ws.Range('I1').ClearContents()
    Write-Output "PAREN formula=$parf val乙=$parok"

    # 5) INDIRECT + 跨表
    $ws.Range('K1').Validation.Delete()
    $ws.Range('K1').Validation.Add(3, 1, 1, '=INDIRECT("主表!A1:A2")')
    $indf = $ws.Range('K1').Validation.Formula1
    $ws.Range('K1').Value2 = 20
    $indok = $ws.Range('K1').Validation.Value
    $ws.Range('K1').ClearContents()
    Write-Output "INDIRECT formula=$indf val20=$indok"

    # 6) 错误样式与信息样式的按钮集合（记录常量：xlValidAlertInformation=3）
    $ws.Range('M1').Validation.Delete()
    $ws.Range('M1').Validation.Add(3, 1, 3, '=$A$1:$A$4')
    Write-Output "INFO alertStyle=$($ws.Range('M1').Validation.AlertStyle)"

    $path = Join-Path $out 'probe.xlsx'
    if (Test-Path $path) { Remove-Item $path -Force }
    $wb.SaveAs($path, 51)
    $wb.Close($false)
    Write-Output "SAVED $path"
}
finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
