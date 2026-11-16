# UI-25 口径实测：Excel 状态栏在「手动隐藏行/列」「筛选隐藏」下的统计
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

function Read-Status($hwnd, $case) {
    Start-Sleep -Milliseconds 700
    $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$hwnd)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::StatusBar)
    $sb = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    if ($null -eq $sb) { Write-Output ("STATUS[{0}] = <no statusbar>" -f $case); return }
    $all = $sb.FindAll([System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
    $names = @()
    foreach ($e in $all) {
        $n = $e.Current.Name
        if ($n -and $n.Trim().Length -gt 0) { $names += $n.Trim() }
    }
    Write-Output ("STATUS[{0}] = {1}" -f $case, ($names -join " || "))
}

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $true
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Add()
$ws = $wb.Worksheets.Item(1)
$ws.Range("A1").Value2 = 1
$ws.Range("A2").Value2 = 2
$ws.Range("A3").Value2 = 3
$ws.Range("A4").Value2 = 4
$ws.Range("B1").Value2 = 5
$ws.Range("C1").Value2 = "v"
$ws.Range("C2").Value2 = 10
$ws.Range("C3").Value2 = 20
$ws.Range("C4").Value2 = 30
$ws.Range("C5").Value2 = 40
$hwnd = $xl.Hwnd
$xl.WindowState = -4143   # xlNormal

# 1 基准：A1:A4 = 1+2+3+4
$ws.Range("A1:A4").Select()
Read-Status $hwnd "baseline A1:A4"

# 2 手动隐藏第 2 行后重选 A1:A4
$ws.Rows(2).Hidden = $true
$ws.Range("A1:A4").Select()
Read-Status $hwnd "manual-hidden-row2 A1:A4"

# 3 取消行隐藏，手动隐藏 B 列，选 A1:B1（1+5=6）
$ws.Rows(2).Hidden = $false
$ws.Columns("B").Hidden = $true
$ws.Range("A1:B1").Select()
Read-Status $hwnd "manual-hidden-colB A1:B1"

# 4 取消列隐藏，筛选 C 列只留 30（C2:C5 = 10,20,30,40）
$ws.Columns("B").Hidden = $false
$ws.Range("C1:C5").AutoFilter(1, 30) | Out-Null
$ws.Range("C2:C5").Select()
Read-Status $hwnd "filter-only-30 C2:C5"

# 5 筛选后同表手动隐藏仍存在：在筛选态下手动隐藏 C4(30) 看是否再变
$ws.Range("C1:C5").AutoFilter() | Out-Null
$ws.Range("C2:C5").Select()
Read-Status $hwnd "filter-off C2:C5"

$wb.Close($false)
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($ws) | Out-Null
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
Write-Output "PROBE_DONE"
