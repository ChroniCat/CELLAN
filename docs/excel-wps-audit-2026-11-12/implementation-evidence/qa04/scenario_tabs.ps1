$root='F:/finance-native-sheets-ready'
. "$root/work/probe/qa04/win_input.ps1"
Copy-Item "$root/work/probe/qa04/fixture.xlsx" "$root/work/probe/qa04/run-T.xlsx" -Force
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList "$root/work/probe/qa04/run-T.xlsx" -PassThru
Start-Sleep -Seconds 4
$w = Find-AppWindow 'run-T'
$all = Get-UiaAll $w
"tabs-before: " + (($all | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.TabItem' } | ForEach-Object { $_.Current.Name }) -join ' / ')
$btn = $all | Where-Object { $_.Current.Name -match '新建标签|新建工作簿|＋|\+' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' } | Select-Object -First 1
"newtab-btn: $($btn.Current.Name)"
if ($btn) { $btn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 800 }
$all2 = Get-UiaAll $w
"tabs-after: " + (($all2 | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.TabItem' } | ForEach-Object { $_.Current.Name }) -join ' / ')
Post-Key $w 'DOWN'
Post-Key $w 'RIGHT'
Start-Sleep -Milliseconds 300
"doc2-sel: $((Get-GridSummary $w))"
$t1 = $all2 | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.TabItem' -and $_.Current.Name -match 'run-T' } | Select-Object -First 1
"switch-target: $($t1.Current.Name)"
if ($t1) {
  try { $t1.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select() }
  catch { try { $t1.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() } catch { "no-select-pattern: $($_.Exception.Message)" } }
  Start-Sleep -Milliseconds 800
}
"doc1-sel: $((Get-GridSummary $w))"
Stop-Process -Id $p.Id -Force
