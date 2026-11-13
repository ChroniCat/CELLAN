# C05：让 Excel / WPS 计算同一个夹具里的比较矩阵，读回结果。
$ErrorActionPreference = 'Stop'
$dir = $args[0].Replace('/', [IO.Path]::DirectorySeparatorChar)
$file = Join-Path $dir 'c05.xlsx'
$out = @()

function Calc-Excel([string]$path) {
    $xl = New-Object -ComObject Excel.Application
    $xl.Visible = $false; $xl.DisplayAlerts = $false
    $wb = $xl.Workbooks.Open($path, 0, $true)
    $ws = $wb.Worksheets.Item(1)
    $rows = @()
    for ($r = 1; $r -le 60; $r++) {
        $f = [string]$ws.Cells.Item($r, 3).Formula
        if ([string]::IsNullOrWhiteSpace($f)) { continue }
        $v = $ws.Cells.Item($r, 3).Text
        $rows += [pscustomobject]@{ row = $r; formula = $f; result = $v }
    }
    $wb.Close($false); $xl.Quit()
    return [pscustomobject]@{ app = 'Excel'; version = $xl.Version; rows = $rows }
}

function Calc-Wps([string]$path) {
    $app = New-Object -ComObject KET.Application
    $app.Visible = $false; $app.DisplayAlerts = $false
    $wb = $app.Workbooks.Open($path)
    $ws = $wb.Worksheets.Item(1)
    try { $ws.Calculate() } catch {}
    $rows = @()
    for ($r = 1; $r -le 60; $r++) {
        $f = [string]$ws.Cells.Item($r, 3).Formula
        if ([string]::IsNullOrWhiteSpace($f)) { continue }
        $rows += [pscustomobject]@{ row = $r; formula = $f; result = [string]$ws.Cells.Item($r, 3).Text }
    }
    $wb.Close($false); $app.Quit()
    return [pscustomobject]@{ app = 'WPS'; version = $app.Version; rows = $rows }
}

$x = Calc-Excel $file
$w = Calc-Wps $file
$meta = [pscustomobject]@{ when = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss zzz'); file = (Split-Path $file -Leaf) }
(@($meta) + @($x) + @($w)) | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 (Join-Path $dir 'probe-c05.json')
"Excel:"; $x.rows | ForEach-Object { "{0} {1} => {2}" -f $_.formula, '', $_.result }
"WPS:";   $w.rows | ForEach-Object { "{0} {1} => {2}" -f $_.formula, '', $_.result }
