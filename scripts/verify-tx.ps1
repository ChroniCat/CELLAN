param(
    [string]$WorktreePath = "",
    [string]$TempDir = "",
    [switch]$SkipEditRegression
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($WorktreePath)) {
    $WorktreePath = Split-Path -Parent $PSScriptRoot
}
$WorktreePath = (Resolve-Path -LiteralPath $WorktreePath).Path

if ([string]::IsNullOrWhiteSpace($TempDir)) {
    $parentDir = Split-Path -Parent $WorktreePath
    $rootDir = [System.IO.Path]::GetPathRoot($WorktreePath)
    if ($parentDir -and ($parentDir -ne $rootDir)) {
        $TempDir = Join-Path $parentDir "tmp"
    } else {
        $TempDir = Join-Path $WorktreePath "tmp"
    }
}

New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
$TempDir = (Resolve-Path -LiteralPath $TempDir).Path

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path -LiteralPath $cargoBin) -and ($env:PATH -notlike "*$cargoBin*")) {
    $env:PATH = "$cargoBin;$env:PATH"
}
$env:TEMP = $TempDir
$env:TMP = $TempDir

$results = New-Object System.Collections.Generic.List[object]
$totalStopwatch = [System.Diagnostics.Stopwatch]::StartNew()

function Invoke-TestSuite {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$CargoArgs,
        [switch]$BuildOnly
    )

    Write-Host ("==> [{0}] cargo {1}" -f $Name, ($CargoArgs -join " ")) -ForegroundColor Cyan
    $sw = [System.Diagnostics.Stopwatch]::StartNew()

    $prevPref = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $output = & cargo @CargoArgs 2>&1
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $prevPref
    }
    $sw.Stop()

    $lines = @($output | ForEach-Object { $_.ToString() })

    if ($BuildOnly) {
        if ($exitCode -ne 0) {
            Write-Host ("FAILED [{0}] (exit={1})" -f $Name, $exitCode) -ForegroundColor Red
            $lines | ForEach-Object { Write-Host $_ }
            exit $exitCode
        }
        Write-Host ("    build ok ({0:F2}s)" -f $sw.Elapsed.TotalSeconds) -ForegroundColor DarkGray
        return
    }

    $passed = 0
    $failed = 0
    $ignored = 0
    $summaryHits = 0
    foreach ($line in $lines) {
        if ($line -match 'test result:\s+\w+\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored;') {
            $passed += [int]$Matches[1]
            $failed += [int]$Matches[2]
            $ignored += [int]$Matches[3]
            $summaryHits += 1
        }
    }

    $status = if ($exitCode -eq 0 -and $failed -eq 0) {
        if ($passed -eq 0) { "WARN(0)" } else { "PASS" }
    } else {
        "FAIL"
    }

    $results.Add([PSCustomObject]@{
        Suite    = $Name
        Passed   = $passed
        Failed   = $failed
        Ignored  = $ignored
        Seconds  = [Math]::Round($sw.Elapsed.TotalSeconds, 2)
        Status   = $status
    })

    if ($status -eq "FAIL") {
        Write-Host ("FAILED [{0}] (exit={1}, passed={2}, failed={3})" -f $Name, $exitCode, $passed, $failed) -ForegroundColor Red
        $lines | ForEach-Object { Write-Host $_ }
        Print-SummaryTable
        exit $(if ($exitCode -ne 0) { $exitCode } else { 1 })
    }

    if ($summaryHits -eq 0) {
        Write-Host ("    WARN: no test summary lines parsed for [{0}]" -f $Name) -ForegroundColor Yellow
    } elseif ($passed -eq 0) {
        Write-Host ("    WARN: 0 tests matched filter [{0}] ({1:F2}s)" -f $Name, $sw.Elapsed.TotalSeconds) -ForegroundColor Yellow
    } else {
        Write-Host ("    ok: {0} passed, {1} ignored ({2:F2}s)" -f $passed, $ignored, $sw.Elapsed.TotalSeconds) -ForegroundColor Green
    }
}

function Print-SummaryTable {
    Write-Host ""
    Write-Host "================ CELLAN Transaction & Invariant Regression Gate ================" -ForegroundColor White
    Write-Host ("Worktree : {0}" -f $WorktreePath)
    Write-Host ("TempDir  : {0}" -f $TempDir)
    Write-Host ""
    Write-Host ("{0,-42} {1,8} {2,8} {3,8} {4,9}  {5}" -f "Suite", "Passed", "Failed", "Ignored", "Time(s)", "Status")
    Write-Host ("{0,-42} {1,8} {2,8} {3,8} {4,9}  {5}" -f ("-" * 42), ("-" * 8), ("-" * 8), ("-" * 8), ("-" * 9), ("-" * 8))

    $sumPassed = 0
    $sumFailed = 0
    $sumIgnored = 0
    foreach ($r in $results) {
        $sumPassed += $r.Passed
        $sumFailed += $r.Failed
        $sumIgnored += $r.Ignored
        $color = switch ($r.Status) {
            "PASS"    { "Green" }
            "FAIL"    { "Red" }
            default   { "Yellow" }
        }
        Write-Host ("{0,-42} {1,8} {2,8} {3,8} {4,9:F2}  {5}" -f $r.Suite, $r.Passed, $r.Failed, $r.Ignored, $r.Seconds, $r.Status) -ForegroundColor $color
    }

    Write-Host ("{0,-42} {1,8} {2,8} {3,8} {4,9}  {5}" -f ("-" * 42), ("-" * 8), ("-" * 8), ("-" * 8), ("-" * 9), ("-" * 8))
    Write-Host ("{0,-42} {1,8} {2,8} {3,8} {4,9:F2}  {5}" -f "TOTAL", $sumPassed, $sumFailed, $sumIgnored, $totalStopwatch.Elapsed.TotalSeconds, $(if ($sumFailed -eq 0) { "PASS" } else { "FAIL" })) -ForegroundColor White
    Write-Host "================================================================================" -ForegroundColor White
}

Push-Location -LiteralPath $WorktreePath
try {
    # 1. Fast fail on core stateful invariant suite first, then full sheetcore regression.
    Invoke-TestSuite -Name "sheetcore::tx_invariants" -CargoArgs @("test", "--offline", "-p", "sheetcore", "--test", "tx_invariants")
    Invoke-TestSuite -Name "sheetcore (all)" -CargoArgs @("test", "--offline", "-p", "sheetcore")

    # 2. Pre-compile app test binary once so per-suite timings reflect test execution.
    Invoke-TestSuite -Name "app (compile)" -CargoArgs @("test", "--offline", "-p", "app", "--no-default-features", "--features", "glow", "--bin", "app", "--no-run") -BuildOnly

    $appSuites = @(
        "phase6_",
        "phase5_",
        "phase4_tx_",
        "phase3_tx_",
        "phase2_app_",
        "tx_exec",
        "cell_shift",
        "rejoin_outbox",
        "review_183",
        "structural",
        "sort",
        "background_paste",
        "csv_import",
        "find_navigation",
        "ai_turn",
        "workflow_tests",
        "diagnostic_cache",
        "comments",
        "spark",
        "chart",
        "pivot",
        "validation",
        "document_tests"
    )

    foreach ($filter in $appSuites) {
        Invoke-TestSuite -Name ("app::{0}" -f $filter) -CargoArgs @(
            "test", "--offline", "-p", "app",
            "--no-default-features", "--features", "glow",
            "--bin", "app", $filter
        )
    }

    if (-not $SkipEditRegression) {
        Invoke-TestSuite -Name "app::edit_regression (excl. ctx_menu)" -CargoArgs @(
            "test", "--offline", "-p", "app",
            "--no-default-features", "--features", "glow",
            "--bin", "app", "edit_regression",
            "--", "--test-threads=1", "--skip", "ctx_menu_rows_match_excel_cell_menu"
        )
    }

    $totalStopwatch.Stop()
    Print-SummaryTable
    exit 0
} finally {
    Pop-Location
}
