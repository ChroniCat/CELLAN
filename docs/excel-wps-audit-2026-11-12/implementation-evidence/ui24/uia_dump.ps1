param([string]$File, [string]$Tag = 'uia', [string]$Action = '')
$root = 'F:/finance-native-sheets-ready'
if ($Action -ne '') { $env:SC_RIBBON_ACTION = $Action } else { Remove-Item Env:SC_RIBBON_ACTION -ErrorAction SilentlyContinue }
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru
Start-Sleep -Seconds 6
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$auto = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $p.Id)
$win = $null
for ($i = 0; $i -lt 20; $i++) {
  $win = $auto.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
  if ($win -ne $null) { break }
  Start-Sleep -Milliseconds 500
}
if ($win -eq $null) {
  "NO_WINDOW pid=$($p.Id)"
} else {
  "WINDOW name='$($win.Current.Name)' class='$($win.Current.ClassName)'"
  $all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
  "COUNT=$($all.Count)"
  foreach ($e in $all) {
    $ct = $e.Current.ControlType.ProgrammaticName -replace 'ControlType\.',''
    $nm = $e.Current.Name
    $aid = $e.Current.AutomationId
    if ($nm -ne '' -or $aid -ne '') {
      "{0}`t{1}`t{2}" -f $ct, $nm, $aid
    }
  }
}
$p.Kill()
