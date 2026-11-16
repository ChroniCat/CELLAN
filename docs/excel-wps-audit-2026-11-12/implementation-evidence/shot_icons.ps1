param([int]$Size=16, [double]$Ppp=1.0)
$root='F:/finance-native-sheets-ready'
$env:SC_ICON_SHEET="$Size"
$env:SC_WIN_W="1280"; $env:SC_WIN_H="800"
$env:SC_PPP="$Ppp"
$env:SC_SHOT="$root/work/probe/ui29/icons-$Size-ppp$Ppp.png"
$env:SC_SHOT_EXIT='1'; $env:SC_SHOT_FRAMES='10'
$p=Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList "$root/work/probe/qa04/fixture.xlsx" -PassThru -RedirectStandardError "$root/work/probe/ui29/icons-$Size.log"
$p.WaitForExit()
if (Test-Path $env:SC_SHOT) { "OK $Size $((Get-Item $env:SC_SHOT).Length)" } else { "FAIL $Size" }
