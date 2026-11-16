param(
  [string]$App = 'F:\finance-native-sheets-ready\target\release\app.exe',
  [string]$Dir = 'F:\finance-native-sheets-ready\work\probe\qa042'
)
$ErrorActionPreference = 'Stop'
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
  $wb = $xl.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Name = 'Sheet1'
  $ws.Range('A1').NumberFormat = '@'
  $ws.Range('A1').Value2 = '00123'
  $ws.Range('B1').Value2 = '中文测试'
  $ws.Range('C1').Value2 = 45000
  $ws.Range('C1').NumberFormat = 'yyyy-mm-dd'
  $ws.Range('D1').Formula = '=1+2'
  $ws.Range('E1').Value2 = '粗体'
  $ws.Range('E1').Font.Bold = $true
  $ws.Range('E1').Interior.Color = 65535
  $ws.Range('E1').Borders.LineStyle = 1
  $ws.Range('A2:B2').Merge()
  $ws.Range('A2').Value2 = '合并单元格'
  $ws.Range('A2:B2').Interior.Color = 15773696
  $ws.Range('A3').Value2 = 42
  $fixture = Join-Path $Dir 'fixture.xlsx'
  $wb.SaveAs($fixture, 51)

  # 黄金样本：Excel 自己把同一份剪贴板粘到 A6
  $ws.Range('A1:E3').Copy() | Out-Null
  Start-Sleep -Milliseconds 350
  $ws.Range('A6').Select()
  $ws.Paste() | Out-Null
  Start-Sleep -Milliseconds 350
  $wb.Save()

  # 再复制一次，确保剪贴板是刚生成的 Excel 格式
  $ws.Range('A1:E3').Copy() | Out-Null
  Start-Sleep -Milliseconds 350

  $env:SC_PASTE_HTML = '1'
  $env:SC_PASTE_AT = '5,0'
  $env:SC_PASTE_DUMP = '5,0,7,4'
  $env:SC_SAVE = (Join-Path $Dir 'app-pasted.xlsx')
  $env:SC_SHOT = (Join-Path $Dir 'app-paste.png')
  $env:SC_SHOT_EXIT = '1'
  $env:SC_SHOT_FRAMES = '10'
  $env:SC_INSTANCE_PORT = '49811'
  $appFile = Join-Path $Dir 'fixture-app.xlsx'
  Copy-Item $fixture $appFile -Force
  $log = Join-Path $Dir 'app-run.log'
  $p = Start-Process -FilePath $App -ArgumentList $appFile -PassThru -RedirectStandardError $log -NoNewWindow
  $p.WaitForExit()
  "app_exit=$($p.ExitCode)"

  # 文件级留证
  Copy-Item $fixture (Join-Path $Dir 'golden.xlsx') -Force
  "saved=golden.xlsx app-pasted.xlsx"
} finally {
  $xl.Quit()
  [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
