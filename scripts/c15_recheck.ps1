param([string]$Path)
$ErrorActionPreference='Continue'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($Path)
$ws=$wb.Worksheets.Item(1)
"protected=" + $ws.ProtectContents
# A1 锁定（应拒绝写入），B1 未锁定（保护开启时仍可写）
try { $ws.Range('A1').Value2 = 'SHOULD_FAIL'; 'A1_WRITE=unexpected_ok' } catch { 'A1_WRITE=denied' }
try { $ws.Range('B1').Value2 = 'INPUT2'; 'B1_WRITE=ok' } catch { 'B1_WRITE=denied' }
try { $ws.Range('B1').Font.Bold = $true; 'B1_FMT=ok' } catch { 'B1_FMT=denied' }
try { $ws.Range('B1').Interior.Color = 65535; 'B1_FILL=ok' } catch { 'B1_FILL=denied' }
"B1_VALUE=" + $ws.Range('B1').Value2
"final_protected=" + $ws.ProtectContents
$wb.Close($false); $xl.Quit()
