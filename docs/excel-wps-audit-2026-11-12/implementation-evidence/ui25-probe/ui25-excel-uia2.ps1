# UI-25 口径实测 2：隐藏列是否计入、计数含文本、分级隐藏行
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
    # 只保留统计项，避免缩放控件噪声
    $pick = $names | Where-Object { $_ -match '^(平均值|计数|数值计数|求和|最小值|最大值)' }
    Write-Output ("STATUS[{0}] = {1}" -f $case, ($pick -join " || "))
}

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $true
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Add()
$ws = $wb.Worksheets.Item(1)
$ws.Range("A1").Value2 = 1
$ws.Range("A2").Value2 = 2
$ws.Range("A3").Value2 = "x"     # 文本：计入「计数」不计入数值统计
$ws.Range("A4").Value2 = 4
$ws.Range("B1").Value2 = 100
$ws.Range("A6").Value2 = 7
$ws.Range("A7").Value2 = 8
$hwnd = $xl.Hwnd
$xl.WindowState = -4143

$ws.Range("A1:A4").Select() | Out-Null
Read-Status $hwnd "baseline-text A1:A4 (1,2,text,4)"

$ws.Columns("B").Hidden = $true
Write-Output ("COLB_HIDDEN=" + $ws.Columns("B").Hidden)
$ws.Range("A1:B1").Select() | Out-Null
Read-Status $hwnd "manual-hidden-colB A1:B1 (1,100)"
$ws.Columns("B").Hidden = $false

$ws.Rows(2).Hidden = $true
Write-Output ("ROW2_HIDDEN=" + $ws.Rows(2).Hidden)
$ws.Range("A1:A4").Select() | Out-Null
Read-Status $hwnd "manual-hidden-row2 A1:A4 (1,text,4)"
$ws.Rows(2).Hidden = $false

# 分级显示（组合）折叠第 7-8 行
$ws.Range("A7:A8").Select() | Out-Null
$ws.Rows("7:8").Group() | Out-Null
$ws.Outline.ShowLevels(1) | Out-Null
Write-Output ("ROW7_HIDDEN_AFTER_OUTLINE=" + $ws.Rows(7).Hidden)
$ws.Range("A6:A8").Select() | Out-Null
Read-Status $hwnd "outline-hidden-rows A6:A8 (7,8)"

$ws.Rows("7:8").Ungroup() | Out-Null
$ws.Range("A6:A8").Select() | Out-Null
Read-Status $hwnd "outline-ungrouped A6:A8 (7,8)"

$wb.Close($false)
$xl.Quit()
Write-Output "PROBE_DONE"
