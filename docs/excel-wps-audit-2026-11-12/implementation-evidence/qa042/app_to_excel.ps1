param(
  [string]$App = 'F:\finance-native-sheets-ready\target\release\app.exe',
  [string]$Dir = 'F:\finance-native-sheets-ready\work\probe\qa042'
)
$ErrorActionPreference = 'Stop'
$src = Join-Path $Dir 'fixture.xlsx'
$appFile = Join-Path $Dir 'app-copy-src.xlsx'
Copy-Item $src $appFile -Force
$log = Join-Path $Dir 'app-copy.log'
$env:SC_RIBBON_SEL = '0,0,2,4'
$env:SC_RIBBON_ACTION = 'Copy'
$env:SC_RIBBON_LOG = '1'
$env:SC_CLIP_LOG = '1'
$env:SC_INSTANCE_PORT = '49812'
$p = Start-Process -FilePath $App -ArgumentList $appFile -PassThru -RedirectStandardError $log -NoNewWindow
Start-Sleep -Seconds 4
# 等 [clip-html] 出现
for ($i = 0; $i -lt 20; $i++) {
  if (Select-String -Path $log -Pattern '\[clip-html\]' -Quiet) { break }
  Start-Sleep -Milliseconds 250
}
"clip_ready=" + (Select-String -Path $log -Pattern '\[clip-html\]' -Quiet)
# 剪贴板用 SetClipboardData 立即写（不是延迟渲染），进程退出数据仍在；
# 先退进程，避免 Excel 粘贴时去问一个正在跑 egui 循环的所有者。
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
  $wb = $xl.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Range('A1').Select()
  $ws.Paste() | Out-Null
  Start-Sleep -Milliseconds 400
  $out = Join-Path $Dir 'excel-pasted.xlsx'
  $wb.SaveAs($out, 51)
  "saved=$out"
  $wb.Close($false)
} finally {
  $xl.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
"app_stopped"
