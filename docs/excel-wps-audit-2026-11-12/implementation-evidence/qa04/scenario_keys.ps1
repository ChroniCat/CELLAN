$root='F:/finance-native-sheets-ready'
. "$root/work/probe/qa04/win_input.ps1"
Copy-Item "$root/work/probe/qa04/fixture.xlsx" "$root/work/probe/qa04/run-K.xlsx" -Force
Set-Clipboard -Value 'hello-native'
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList "$root/work/probe/qa04/run-K.xlsx" -PassThru
Start-Sleep -Seconds 4
$w = Find-AppWindow 'run-K'
"start: $((Get-ActiveCellName $w))"
Post-Key $w 'V' -Ctrl
Start-Sleep -Milliseconds 600
"after-paste: $((Get-ActiveCellName $w))"
Post-Key $w 'Z' -Ctrl
Start-Sleep -Milliseconds 500
"after-undo: $((Get-ActiveCellName $w))"
Post-Key $w 'Y' -Ctrl
Start-Sleep -Milliseconds 500
"after-redo: $((Get-ActiveCellName $w))"
$b=(Get-Item "$root/work/probe/qa04/run-K.xlsx").LastWriteTime
Post-Key $w 'S' -Ctrl
Start-Sleep -Seconds 3
$a=(Get-Item "$root/work/probe/qa04/run-K.xlsx").LastWriteTime
"saved=$($a -ne $b)"
Stop-Process -Id $p.Id -Force
