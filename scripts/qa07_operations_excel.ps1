# Compare with perf_probe --operations on its pristine, generated workbook.
param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'
$file = (Resolve-Path -LiteralPath $Path).Path
$excel = New-Object -ComObject Excel.Application
$book = $null
try {
    $excel.Visible = $false
    $excel.DisplayAlerts = $false
    $excel.AutomationSecurity = 3
    $t = [Diagnostics.Stopwatch]::StartNew()
    $book = $excel.Workbooks.Open($file, 0, $true)
    $openMs = $t.Elapsed.TotalMilliseconds
    $sheet = $book.Worksheets.Item(1)
    $rows = $sheet.UsedRange.Rows.Count - 1
    if ($sheet.Range('A1').Text -ne 'Key' -or $sheet.Range('B1').Text -ne 'Group' -or
        $sheet.Range('A2').Value2 -ne $rows) { throw 'Unexpected operations fixture' }
    $last = $rows + 1
    $range = $sheet.Range("A1:B$last")
    $t.Restart()
    $null = $range.Sort($sheet.Range('A2'), 1, [Type]::Missing, [Type]::Missing, 1, [Type]::Missing, 1, 1)
    $sortMs = $t.Elapsed.TotalMilliseconds
    if ($sheet.Range('A2').Value2 -ne 1 -or $sheet.Range("A$last").Value2 -ne $rows) { throw 'Sort failed' }
    $at = [int][Math]::Floor($rows/2) + 2
    $t.Restart()
    $null = $sheet.Rows.Item($at).Insert()
    $insertMs = $t.Elapsed.TotalMilliseconds
    $last++
    $t.Restart()
    $null = $sheet.Range("A1:B$last").AutoFilter(2, '=1')
    $filterMs = $t.Elapsed.TotalMilliseconds
    $visible = $excel.WorksheetFunction.Subtotal(103, $sheet.Range("A2:A$last"))
    if ($visible -ne [Math]::Ceiling($rows/2)) { throw 'Filter count differs' }
    [pscustomobject]@{product='Excel';version=$excel.Version;rows=$rows;open_ms=$openMs;
        sort_ms=$sortMs;insert_ms=$insertMs;filter_ms=$filterMs;visible_data_rows=$visible} | ConvertTo-Json -Compress
} finally {
    if ($book) { $book.Close($false) }
    $excel.Quit()
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($excel)
}
