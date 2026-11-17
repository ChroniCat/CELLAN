param([string]$In,[string]$Dest)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($In)
$ws=$wb.Worksheets.Item(1)
$n=$ws.UsedRange.Rows.Count
"rows=$n"
$rng=$ws.Range("A1:A$n")
$ws.Sort.SortFields.Clear()
$null=$ws.Sort.SortFields.Add($rng,0,1)
$ws.Sort.SetRange($rng)
$ws.Sort.Header=2
$ws.Sort.SortMethod=2
$ws.Sort.Apply()
if(Test-Path $Dest){Remove-Item $Dest -Force}
$wb.SaveAs($Dest,51)
$wb.Close($false); $xl.Quit(); 'ok'
