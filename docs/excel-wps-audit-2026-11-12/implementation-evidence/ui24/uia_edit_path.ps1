param([string]$File)
$root = 'F:/finance-native-sheets-ready'
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru
Start-Sleep -Seconds 6
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$auto = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $p.Id)
$win = $auto.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
$all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
foreach ($e in $all) {
  if ($e.Current.ControlType.ProgrammaticName -ne 'ControlType.Edit') { continue }
  $path = @()
  $cur = $e
  for ($i = 0; $i -lt 5; $i++) {
    $cur = $walker.GetParent($cur)
    if ($null -eq $cur) { break }
    $path += ('{0}:{1}' -f ($cur.Current.ControlType.ProgrammaticName -replace 'ControlType\.',''), $cur.Current.Name)
  }
  ('EDIT name="{0}" value-len={1} path={2}' -f $e.Current.Name, $e.Current.Name.Length, ($path -join ' <- '))
}
$p.Kill()
