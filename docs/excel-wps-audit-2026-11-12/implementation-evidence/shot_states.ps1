param([string]$Tag, [string]$EnvName, [string]$File='F:/finance-native-sheets-ready/work/probe/qa04/fixture.xlsx')
$root='F:/finance-native-sheets-ready'
$env:SC_WIN_W='1280'; $env:SC_WIN_H='800'
Set-Item -Path "Env:$EnvName" -Value '1'
$env:SC_SHOT="$root/work/probe/ui29/$Tag.png"
$env:SC_SHOT_EXIT='1'; $env:SC_SHOT_FRAMES='16'
$p=Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru -RedirectStandardError "$root/work/probe/ui29/$Tag.log"
$p.WaitForExit()
Remove-Item "Env:$EnvName" -ErrorAction SilentlyContinue
if (Test-Path $env:SC_SHOT) { "OK $Tag $((Get-Item $env:SC_SHOT).Length)" } else { "FAIL $Tag" }
