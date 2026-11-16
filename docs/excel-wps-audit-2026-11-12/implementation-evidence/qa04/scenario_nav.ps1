$root='F:/finance-native-sheets-ready'
. "$root/work/probe/qa04/win_input.ps1"
Copy-Item "$root/work/probe/qa04/fixture.xlsx" "$root/work/probe/qa04/run-N.xlsx" -Force
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList "$root/work/probe/qa04/run-N.xlsx" -PassThru
Start-Sleep -Seconds 4
$w = Find-AppWindow 'run-N'
"fg=$([QI.Win]::MapVirtualKey(0,0)) active=$((Get-ActiveCellName $w))"
Post-Key $w 'F2'
Start-Sleep -Milliseconds 300
"edit-node-after-F2: $((Get-EditValue $w))"
Post-Key $w 'ESC'
Start-Sleep -Milliseconds 300
"edit-node-after-ESC: $((Get-EditValue $w))"
$g0 = Get-GridSummary $w
Post-Key $w 'ENTER'
Start-Sleep -Milliseconds 300
"after-enter: $g0 -> $((Get-GridSummary $w))"
Post-Key $w 'DOWN'
Post-Key $w 'DOWN'
Post-Key $w 'RIGHT'
Start-Sleep -Milliseconds 300
"after-down-down-right: $((Get-GridSummary $w))"
Stop-Process -Id $p.Id -Force
