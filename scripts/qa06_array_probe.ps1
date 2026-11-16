# QA-06.2：把 anchor.tsv 里的数组公式写进 Excel 实算，读回溢出块，导出 blocks.tsv。
# blocks.tsv 每行：anchor<TAB>formula<TAB>rows<TAB>cols<TAB>v(0,0)<TAB>v(0,1)<TAB>...
# 值统一用 Value2 的字符串形式；错误值用显示文本（#N/A 等）。
$ErrorActionPreference = 'Stop'
$dir = $args[0]
if ([string]::IsNullOrWhiteSpace($dir)) { $dir = 'work/qa06a' }
$dir = $dir.Replace('/', [IO.Path]::DirectorySeparatorChar)
$file = Join-Path $dir 'qa06a.xlsx'
$anchors = Get-Content (Join-Path $dir 'anchor.tsv') -Encoding UTF8 | Where-Object { $_.Trim() -ne '' }

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false; $xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($file, 0, $false)
$ws = $wb.Worksheets.Item(1)

foreach ($line in $anchors) {
    $parts = $line.Split("`t")
    $addr = $parts[0]
    $formula = $parts[1]
    $ws.Range($addr).Formula2 = $formula
}
$try = 0
while ($true) {
    try { $ws.CalculateUntilAsyncQueriesDone(); break } catch { $try++; if ($try -gt 3) { break } }
}
$ws.Calculate()

function CellVal($cell) {
    $v = $null
    try { $v = $cell.Value2 } catch { return [string]$cell.Text }
    if ($null -eq $v) { return '' }
    if ($v -is [bool]) { if ($v) { return 'TRUE' } else { return 'FALSE' } }
    if ($v -is [string]) { return $v }
    if ($cell.Text -like '#*') { return [string]$cell.Text }
    return [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:R}', [double]$v)
}

$out = @()
foreach ($line in $anchors) {
    $parts = $line.Split("`t")
    $addr = $parts[0]
    $formula = $parts[1]
    $cell = $ws.Range($addr)
    $rng = $null
    if ($cell.HasSpill) { $rng = $cell.SpillingToRange } else { $rng = $cell }
    $rows = $rng.Rows.Count
    $cols = $rng.Columns.Count
    $vals = @()
    for ($r = 1; $r -le $rows; $r++) {
        for ($c = 1; $c -le $cols; $c++) {
            $vals += (CellVal $rng.Cells.Item($r, $c))
        }
    }
    $out += [pscustomobject]@{
        anchor  = $addr
        formula = [string]$cell.Formula2
        rows    = $rows
        cols    = $cols
        values  = $vals
    }
}

$lines = @()
foreach ($o in $out) {
    $lines += (($o.anchor, $o.formula, $o.rows, $o.cols) + $o.values) -join "`t"
}
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllLines((Join-Path $dir 'blocks.tsv'), $lines, $utf8NoBom)

# ---- 溢出冲突：读被占位时的锚点文本，再清掉占位重算，读回真实溢出块 ----
$blockedFile = Join-Path $dir 'blocked.tsv'
if (Test-Path $blockedFile) {
    $blocked = @(Get-Content $blockedFile -Encoding UTF8 | Where-Object { $_.Trim() -ne '' })
    $conflictLines = @()
    foreach ($line in $blocked) {
        $ws.Range($line.Split("`t")[0]).Formula2 = $line.Split("`t")[2]
    }
    $ws.Calculate()
    foreach ($line in $blocked) {
        $parts = $line.Split("`t")
        $addr = $parts[0]
        $blocker = $parts[1]
        $formula = $parts[2]
        $ws.Range($addr).Formula2 = $formula
        $cell = $ws.Range($addr)
        $has = $false
        try { $has = $cell.HasSpill } catch {}
        $conflictLines += (($addr, [string]$cell.Text, [string]$has) -join "`t")
    }
    [IO.File]::WriteAllLines((Join-Path $dir 'spill_conflict.tsv'), $conflictLines, $utf8NoBom)

    foreach ($line in $blocked) {
        $parts = $line.Split("`t")
        $ws.Range($parts[1]).ClearContents() | Out-Null
    }
    $ws.Calculate()
    $cleared = @()
    foreach ($line in $blocked) {
        $parts = $line.Split("`t")
        $cell = $ws.Range($parts[0])
        $rng = $null
        if ($cell.HasSpill) { $rng = $cell.SpillingToRange } else { $rng = $cell }
        $rows = $rng.Rows.Count
        $cols = $rng.Columns.Count
        $vals = @()
        for ($r = 1; $r -le $rows; $r++) {
            for ($c = 1; $c -le $cols; $c++) {
                $vals += (CellVal $rng.Cells.Item($r, $c))
            }
        }
        $cleared += [pscustomobject]@{
            anchor  = $parts[0]
            formula = [string]$cell.Formula2
            rows    = $rows
            cols    = $cols
            values  = $vals
        }
    }
    $clearLines = @()
    foreach ($o in $cleared) {
        $clearLines += (($o.anchor, $o.formula, $o.rows, $o.cols) + $o.values) -join "`t"
    }
    [IO.File]::WriteAllLines((Join-Path $dir 'spill_cleared.tsv'), $clearLines, $utf8NoBom)
    "溢出冲突 $($blocked.Count) 条：$(($conflictLines) -join ' | ')"
}

$meta = [pscustomobject]@{
    when  = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss zzz')
    excel = $xl.Version
    cases = $out.Count
}
$meta | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $dir 'probe-meta.json')

$wb.Close($false)
$xl.Quit()
"Excel $($xl.Version)：$($out.Count) 个数组块"
$lines | ForEach-Object { $_ }
