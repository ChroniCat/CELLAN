# QA-06：让 Excel（和可用的 WPS）实算夹具里的公式，导出 group<TAB>formula<TAB>显示值<TAB>Value2。
$ErrorActionPreference = 'Stop'
$dir = $args[0]
if ([string]::IsNullOrWhiteSpace($dir)) { $dir = 'work/qa06' }
$dir = $dir.Replace('/', [IO.Path]::DirectorySeparatorChar)
$casesPath = Join-Path $dir 'cases.json'
$file = Join-Path $dir 'qa06.xlsx'
$cases = Get-Content $casesPath -Raw -Encoding UTF8 | ConvertFrom-Json
$formulas = @($cases.cases | ForEach-Object { $_.formula })
$groups = @($cases.cases | ForEach-Object { $_.group })

function Probe([string]$exe, [string]$appName) {
    $xl = New-Object -ComObject $exe
    $xl.Visible = $false; $xl.DisplayAlerts = $false
    $wb = $xl.Workbooks.Open($file, 0, $false)
    $ws = $wb.Worksheets.Item(1)
    # 公式交给 Excel 自己解析填入，避免 openpyxl 漏 _xlfn 前缀把新函数写成 #NAME?
    for ($i = 0; $i -lt $formulas.Count; $i++) {
        $ws.Cells.Item($i + 1, 3).Formula = $formulas[$i]
    }
    try { $ws.Calculate() } catch {}
    $rows = @()
    for ($i = 0; $i -lt $formulas.Count; $i++) {
        $r = $i + 1
        $cell = $ws.Cells.Item($r, 3)
        $v2 = $null
        try { $v2 = $cell.Value2 } catch {}
        $rows += [pscustomobject]@{
            group   = $groups[$i]
            formula = [string]$cell.Formula
            text    = [string]$cell.Text
            value2  = if ($null -eq $v2) {
                ''
            } elseif ($v2 -is [bool]) {
                if ($v2) { 'TRUE' } else { 'FALSE' }
            } elseif ($v2 -is [string]) {
                $v2
            } else {
                [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0:R}', [double]$v2)
            }
        }
    }
    $ver = $xl.Version
    $wb.Close($false)
    $xl.Quit()
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($ws)
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($wb)
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
    [GC]::Collect()
    return [pscustomobject]@{ app = $appName; version = $ver; rows = $rows }
}

$excel = Probe 'Excel.Application' 'Excel'
$excel.rows | ForEach-Object { "{0}`t{1}`t{2}`t{3}" -f $_.group, $_.formula, $_.text, $_.value2 } |
    Set-Content -Encoding UTF8 (Join-Path $dir 'excel.tsv')

$wpsOk = $false
try {
    $wps = Probe 'KET.Application' 'WPS'
    $wps.rows | ForEach-Object { "{0}`t{1}`t{2}`t{3}" -f $_.group, $_.formula, $_.text, $_.value2 } |
        Set-Content -Encoding UTF8 (Join-Path $dir 'wps.tsv')
    $wpsOk = $true
} catch {
    "WPS 不可用：$($_.Exception.Message)"
}

$meta = [pscustomobject]@{
    when    = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss zzz')
    excel   = $excel.version
    wps     = if ($wpsOk) { $wps.version } else { $null }
    cases   = $formulas.Count
}
$meta | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $dir 'probe-meta.json')
"Excel $($excel.version)：$($formulas.Count) 条；WPS=" + $(if ($wpsOk) { $wps.version } else { 'n/a' })
