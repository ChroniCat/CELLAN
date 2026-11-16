param(
  [string]$App = 'F:\finance-native-sheets-ready\target\release\app.exe',
  [string]$Dir = 'F:\finance-native-sheets-ready\work\probe\qa042'
)
$ErrorActionPreference = 'Stop'
$et = New-Object -ComObject KET.Application
$et.Visible = $false
$et.DisplayAlerts = $false
try {
  $wb = $et.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Range('A1').NumberFormat = '@'
  $ws.Range('A1').Value2 = '00123'
  $ws.Range('B1').Value2 = '中文测试'
  $ws.Range('C1').Value2 = 45000
  $ws.Range('C1').NumberFormat = 'yyyy-mm-dd'
  $ws.Range('D1').Formula = '=1+2'
  $ws.Range('E1').Value2 = '粗体'
  $ws.Range('E1').Font.Bold = $true
  $ws.Range('E1').Interior.Color = 65535
  $ws.Range('A2:B2').Merge()
  $ws.Range('A2').Value2 = '合并单元格'
  $ws.Range('A2:B2').Interior.Color = 15773696
  $ws.Range('A3').Value2 = 42
  $fixture = Join-Path $Dir 'wps-fixture.xlsx'
  $wb.SaveAs($fixture, 51)

  $ws.Range('A1:E3').Copy() | Out-Null
  Start-Sleep -Milliseconds 500
  $ws.Range('A6').Select()
  try { $ws.Paste() | Out-Null } catch { "wps_self_paste=fail $($_.Exception.Message)" }
  Start-Sleep -Milliseconds 500
  $wb.Save()

  $ws.Range('A1:E3').Copy() | Out-Null
  Start-Sleep -Milliseconds 500

  $env:SC_PASTE_HTML = '1'
  $env:SC_PASTE_AT = '5,0'
  $env:SC_PASTE_DUMP = '5,0,7,4'
  $env:SC_SAVE = (Join-Path $Dir 'wps-app-pasted.xlsx')
  $env:SC_SHOT = (Join-Path $Dir 'wps-app-paste.png')
  $env:SC_SHOT_EXIT = '1'
  $env:SC_SHOT_FRAMES = '10'
  $env:SC_INSTANCE_PORT = '49841'
  $appFile = Join-Path $Dir 'wps-fixture-app.xlsx'
  Copy-Item $fixture $appFile -Force
  $log = Join-Path $Dir 'wps-app-run.log'
  $p = Start-Process -FilePath $App -ArgumentList $appFile -PassThru -RedirectStandardError $log -NoNewWindow
  $p.WaitForExit()
  "app_exit=$($p.ExitCode)"
  Copy-Item $fixture (Join-Path $Dir 'wps-golden.xlsx') -Force
  "saved=wps-golden.xlsx wps-app-pasted.xlsx"
} finally {
  $et.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($et)
}
