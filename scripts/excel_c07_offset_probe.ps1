$ErrorActionPreference = "Stop"
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $s1 = $wb.Worksheets.Item(1)
    $s1.Name = "主表"
    $s2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $s1)
    $s2.Name = "数据"
    $s2.Range("A1").Value2 = 100
    $s2.Range("A2").Value2 = 200
    $s2.Range("A3").Value2 = 300
    $s2.Range("A4").Value2 = 400
    $s2.Range("A5").Value2 = 500
    $s1.Range("A1").Value2 = 1
    $s1.Range("A2").Value2 = 2
    $tests = @(
        @{ n = "rows-omit-h"; f = '=ROWS(OFFSET($A$1:$A$3,0,0))' },
        @{ n = "cols-omit-w"; f = '=COLUMNS(OFFSET($A$1:$A$3,0,0))' },
        @{ n = "shift-1"; f = '=ROWS(OFFSET($A$1:$A$3,1,0))' },
        @{ n = "neg-h"; f = '=ROWS(OFFSET($A$3,0,0,-2,1))' },
        @{ n = "neg-h-top"; f = '=ADDRESS(ROW(OFFSET($A$3,0,0,-2,1)),COLUMN(OFFSET($A$3,0,0,-2,1)))' },
        @{ n = "cross-count"; f = '=ROWS(OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1))' },
        @{ n = "cross-first"; f = '=ADDRESS(ROW(OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1)),COLUMN(OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1)))' }
    )
    foreach ($t in $tests) {
        try {
            $v = $s1.Evaluate($t.f)
            if ($v -is [System.Array]) { $v = ($v -join ",") }
            "$($t.n) = $v"
        } catch { "$($t.n) ERR $($_.Exception.Message)" }
    }
    $dv1 = $s1.Range("C1:C2").Validation
    $dv1.Delete()
    $dv1.Add(3, 1, 1, '=OFFSET($A$1:$A$3,0,0)')
    "dv1 formula=$($dv1.Formula1)"
    $s1.Range("C1").Value2 = 2
    "dv1 C1=2 valid=$($s1.Range('C1').Validation.Value)"
    $dv2 = $s1.Range("D1").Validation
    $dv2.Delete()
    $dv2.Add(3, 1, 1, '=OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1)')
    "dv2 formula=$($dv2.Formula1)"
    $s1.Range("D1").Value2 = 200
    "dv2 D1=200 valid=$($s1.Range('D1').Validation.Value)"
    $s1.Range("D1").Value2 = 400
    "dv2 D1=400 valid=$($s1.Range('D1').Validation.Value)"
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
