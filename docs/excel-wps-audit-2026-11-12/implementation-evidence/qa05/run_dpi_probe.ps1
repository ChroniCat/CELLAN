param([string]$File, [string]$Tag = 'dpi', [string]$Ppps = '1.0,1.25,1.5,2.0', [int]$W = 1440, [int]$H = 900)
$root = 'F:/finance-native-sheets-ready'
# 逻辑尺寸固定 W×H 点：物理像素 = W×ppp，等价于高 DPI 屏幕（界面物理尺寸一致、像素更密）
$env:SC_SHOT_EXIT = '1'
$env:SC_SHOT_FRAMES = '12'
$out = @()
foreach ($ppp in $Ppps.Split(',')) {
  $env:SC_PPP = $ppp
  $env:SC_WIN_W = "$([math]::Round($W * [double]$ppp))"
  $env:SC_WIN_H = "$([math]::Round($H * [double]$ppp))"
  $env:SC_SHOT = "$root/work/probe/qa05/$Tag-$ppp.png"
  $p = Start-Process -FilePath "$root/target/release/app.exe" -ArgumentList $File -PassThru -RedirectStandardError "$root/work/probe/qa05/$Tag-$ppp.log"
  $p.WaitForExit()
  if (Test-Path $env:SC_SHOT) {
    $out += "ppp=$ppp shot=$env:SC_SHOT"
  } else {
    $out += "ppp=$ppp NO_SHOT log=$root/work/probe/qa05/$Tag-$ppp.log"
  }
}
$out
