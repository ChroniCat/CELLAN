param([string]$File, [string]$Sel = '1,1', [string]$Sel2 = '3,2')
$root = 'F:/finance-native-sheets-ready'
$env:SC_SEL = $Sel
$env:SC_SEL2 = $Sel2
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru
Start-Sleep -Seconds 6
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$auto = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $p.Id)
$win = $auto.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
if ($win -eq $null) { "NO_WINDOW"; $p.Kill(); exit 1 }
$all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
foreach ($e in $all) {
  $nm = $e.Current.Name
  if ($nm.StartsWith('列 ') -or $nm.StartsWith('第 ')) { } else { continue }
    $r = $e.Current.BoundingRectangle
    "{0}`tbounds={1},{2},{3},{4}" -f $nm, $r.X, $r.Y, $r.Width, $r.Height
}
$p.Kill()
