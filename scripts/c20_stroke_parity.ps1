param([string]$In,[string]$Dest)
$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application; $xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open($In)
$ws=$wb.Worksheets.Item(1)
$n=$ws.UsedRange.Rows.Count
$rng=$ws.Range("A1:A$n")
$ws.Sort.SortFields.Clear()
$null=$ws.Sort.SortFields.Add($rng,0,1)
$ws.Sort.SetRange($rng)
$ws.Sort.Header=2
$ws.Sort.SortMethod=2
$ws.Sort.Apply()
$out=@(); for($i=1;$i -le $n;$i++){ $out += [string]$ws.Cells.Item($i,1).Value2 }
[IO.File]::WriteAllLines($Dest,$out,[Text.UTF8Encoding]::new($false))
$wb.Close($false); $xl.Quit(); "sorted $n"
