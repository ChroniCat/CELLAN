param([string]$Tag = 'B', [string]$Mode = 'esc')
$root = 'F:/finance-native-sheets-ready'
. "$root/work/probe/qa04/input_lib.ps1"
Copy-Item "$root/work/probe/qa04/fixture.xlsx" "$root/work/probe/qa04/run-$Tag.xlsx" -Force
$p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList "$root/work/probe/qa04/run-$Tag.xlsx" -PassThru
Start-Sleep -Seconds 4
$win = Focus-App "run-$Tag"
$r = Get-CellRect $win 'A1'
if ($r -eq $null) { "NO_CELL_RECT"; exit 1 }
"cell A1 rect=$($r.X),$($r.Y),$($r.Width),$($r.Height)"
Click-At ([int]($r.X + $r.Width/2)) ([int]($r.Y + $r.Height/2))
Start-Sleep -Milliseconds 300
"before: $((Get-ActiveCellName $win))"
VkPress 'F2'
Start-Sleep -Milliseconds 250
"editing: $((Get-ActiveCellName $win))"
Send-UnicodeText 'abc'
VkPress 'LEFT' 2
Send-UnicodeText 'X'
if ($Mode -eq 'esc') { VkPress 'ESC' } else { VkPress 'ENTER' }
Start-Sleep -Milliseconds 400
"after-edit: $((Get-ActiveCellName $win))"
$before = (Get-Item "$root/work/probe/qa04/run-$Tag.xlsx").LastWriteTime
CtrlKey 'S'
Start-Sleep -Seconds 3
$after = (Get-Item "$root/work/probe/qa04/run-$Tag.xlsx").LastWriteTime
"saved=$($after -ne $before)"
Stop-Process -Id $p.Id -Force
"done $Tag/$Mode"
