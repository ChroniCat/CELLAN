param([string]$Tag, [string]$File='F:/finance-native-sheets-ready/work/probe/qa04/fixture.xlsx', [double]$Ppp=1.0, [int]$W=1280, [int]$H=800)
$root='F:/finance-native-sheets-ready'
$env:SC_PPP="$Ppp"; $env:SC_WIN_W="$W"; $env:SC_WIN_H="$H"
$env:SC_SHOT="$root/work/probe/qa05/$Tag.png"
$env:SC_SHOT_EXIT='1'; $env:SC_SHOT_FRAMES='14'
$p=Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru -RedirectStandardError "$root/work/probe/qa05/$Tag.log"
$p.WaitForExit()
if (Test-Path $env:SC_SHOT) { "SHOT_OK $((Get-Item $env:SC_SHOT).Length) bytes" } else { 'NO_SHOT' }
