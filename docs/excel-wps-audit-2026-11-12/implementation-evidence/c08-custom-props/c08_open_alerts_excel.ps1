$ErrorActionPreference = 'Stop'
$src = 'F:/finance-native-sheets-ready/work/c08_verify/app_saved.xlsx'
$dst = 'C:/Users/Administrator/AppData/Local/hermes/cache/scratch/c08_open_excel.xlsx'
Copy-Item -LiteralPath $src -Destination $dst -Force
$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $dst).Hash.ToLower()
Write-Output ("COPY_SHA256=" + $sha)
$x = New-Object -ComObject Excel.Application
$x.Visible = $true
$x.DisplayAlerts = $true
$x.AskToUpdateLinks = $false
Write-Output ("ALERTS_ENABLED=" + $x.DisplayAlerts)
$sw = [Diagnostics.Stopwatch]::StartNew()
try {
  $wb = $x.Workbooks.Open($dst, 0, $false)
  $sw.Stop()
  Write-Output ("OPEN_OK elapsed_ms=" + $sw.ElapsedMilliseconds)
  Write-Output ("WB name=" + $wb.Name + " sheets=" + $wb.Sheets.Count + " saved=" + $wb.Saved)
  foreach ($w in $x.Windows) { Write-Output ("EXCEL_WINDOW=" + $w.Caption) }
  $wb.Close($false)
  Write-Output ("CLOSE_OK alerts=" + $x.DisplayAlerts)
} catch {
  $sw.Stop()
  Write-Output ("OPEN_ERROR elapsed_ms=" + $sw.ElapsedMilliseconds + " " + $_.Exception.Message)
  foreach ($w in $x.Windows) { Write-Output ("ERROR_WINDOW=" + $w.Caption) }
}
$x.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($x) | Out-Null
