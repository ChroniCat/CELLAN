param([string]$File, [string]$Tag = 'size', [string]$Sizes = '1440x900,1024x768,760x420')
$root = 'F:/finance-native-sheets-ready'
$env:SC_PPP = '1.0'
$env:SC_SHOT_EXIT = '1'
$env:SC_SHOT_FRAMES = '12'
$out = @()
foreach ($sz in $Sizes.Split(',')) {
  $wh = $sz.Split('x')
  $env:SC_WIN_W = $wh[0]
  $env:SC_WIN_H = $wh[1]
  $env:SC_SHOT = "$root/work/probe/qa05/$Tag-$sz.png"
  $p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru -RedirectStandardError "$root/work/probe/qa05/$Tag-$sz.log"
  $p.WaitForExit()
  if (Test-Path $env:SC_SHOT) { $out += "size=$sz shot=ok" } else { $out += "size=$sz NO_SHOT (see $Tag-$sz.log)" }
}
$out
