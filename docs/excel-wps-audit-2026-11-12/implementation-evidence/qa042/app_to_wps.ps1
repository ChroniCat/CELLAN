param(
  [string]$App = 'F:\finance-native-sheets-ready\target\release\app.exe',
  [string]$Dir = 'F:\finance-native-sheets-ready\work\probe\qa042'
)
$ErrorActionPreference = 'Stop'
$appFile = Join-Path $Dir 'wps-copy-src.xlsx'
Copy-Item (Join-Path $Dir 'wps-fixture.xlsx') $appFile -Force
$log = Join-Path $Dir 'wps-copy.log'
$env:SC_RIBBON_SEL = '0,0,2,4'
$env:SC_RIBBON_ACTION = 'Copy'
$env:SC_RIBBON_LOG = '1'
$env:SC_INSTANCE_PORT = '49842'
$p = Start-Process -FilePath $App -ArgumentList $appFile -PassThru -RedirectStandardError $log -NoNewWindow
Start-Sleep -Seconds 4
for ($i = 0; $i -lt 20; $i++) {
  if (Select-String -Path $log -Pattern '\[clip\]' -Quiet) { break }
  Start-Sleep -Milliseconds 250
}
"clip_log=" + (Select-String -Path $log -Pattern '\[clip\]' -Quiet)

$et = New-Object -ComObject KET.Application
$et.Visible = $false
$et.DisplayAlerts = $false
try {
  $wb = $et.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Range('A1').Select()
  try {
    $ws.Paste() | Out-Null
    Start-Sleep -Milliseconds 500
    "wps_paste=ok A1=$($ws.Range('A1').Value2) D1f=$($ws.Range('D1').Formula)"
    $out = Join-Path $Dir 'wps-pasted.xlsx'
    $wb.SaveAs($out, 51)
  } catch {
    "wps_paste=FAIL $($_.Exception.Message)"
  }
  $wb.Close($false)
} finally {
  $et.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($et)
}
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
"app_stopped"
