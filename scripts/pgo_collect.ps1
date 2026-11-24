<#
.SYNOPSIS
    CELLAN PGO（Profile-Guided Optimization）热路径样本采集与合并脚本。

.DESCRIPTION
    在 D 盘工作目录执行完整的 PGO 第一阶段采集流程：
      1. 定位本机 Rust 工具链的 llvm-profdata.exe（需已安装 llvm-tools-preview）；
      2. 使用 -C profile-generate=<profraw_dir>（保留 +crt-static 与 target-cpu=x86-64-v2）构建插桩产物；
      3. 运行 sheetcore 核心压测（perf_wave_bench）与 app 无界面压测（perf_wave_app_suite）采集热路径样本；
      4. 调用 llvm-profdata merge 合并生成 cellan.profdata；
      5. 输出可直接交给 scripts\publish.ps1 -PgoProfdata 使用的发版命令。

.EXAMPLE
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\pgo_collect.ps1

.EXAMPLE
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\pgo_collect.ps1 -WorkDir D:\CELLAN-build\pgo -BenchFilter date_format
#>
param(
    [string]$WorkDir = "D:\CELLAN-build\pgo",
    [string]$Output = "",
    [string]$BenchFilter = "",
    [switch]$SkipCoreBench,
    [switch]$SkipAppBench,
    [switch]$Online,
    [switch]$Help
)

$ErrorActionPreference = "Stop"

function Show-Usage {
    @"
CELLAN PGO Profile Collector (scripts/pgo_collect.ps1)

用法:
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\pgo_collect.ps1 [参数]

参数:
  -WorkDir <目录>      PGO 临时构建与 profraw 目录（默认: D:\CELLAN-build\pgo，路径不能包含空格）
  -Output <文件>       合并后的 .profdata 输出路径（默认: <WorkDir>\cellan.profdata）
  -BenchFilter <名称>  仅运行匹配的 perf_wave_bench 子项（留空运行全套）
  -SkipCoreBench       跳过 sheetcore perf_wave_bench 采集
  -SkipAppBench        跳过 app perf_wave_app_suite 无界面采集
  -Online              允许 Cargo 联网解析（默认附加 --offline --locked）
  -Help                显示本帮助信息

前置要求:
  rustup component add llvm-tools-preview

采集完成后，将生成的 .profdata 传给发版脚本启用 PGO 编译:
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\publish.ps1 "" "<更新说明>" -PgoProfdata "<cellan.profdata>"
"@ | Write-Host
}

if ($Help) {
    Show-Usage
    exit 0
}

function Find-LlvmProfdata {
    $sysroot = (& rustc --print sysroot).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($sysroot)) {
        throw "Unable to run 'rustc --print sysroot'. Ensure the Rust MSVC toolchain is installed."
    }
    $hostTriple = ""
    foreach ($line in (& rustc -vV)) {
        if ($line -match '^host:\s*(\S+)') {
            $hostTriple = $Matches[1]
            break
        }
    }
    $candidates = @()
    if ($hostTriple) {
        $candidates += Join-Path $sysroot "lib\rustlib\$hostTriple\bin\llvm-profdata.exe"
    }
    $cmd = Get-Command llvm-profdata -ErrorAction SilentlyContinue
    if ($cmd) {
        $candidates += $cmd.Source
    }
    $candidates += @(
        "C:\Program Files\LLVM\bin\llvm-profdata.exe",
        "C:\Program Files (x86)\LLVM\bin\llvm-profdata.exe"
    )
    foreach ($p in $candidates) {
        if ($p -and (Test-Path -LiteralPath $p)) {
            return (Resolve-Path -LiteralPath $p).Path
        }
    }
    throw @"
Cannot find llvm-profdata.exe in Rust sysroot ($sysroot) or PATH.
Install the Rust LLVM tools component first:
  rustup component add llvm-tools-preview
"@
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $cargoBin) -and ($env:PATH -notlike "*$cargoBin*")) {
    $env:PATH = "$cargoBin;$env:PATH"
}

if ($SkipCoreBench -and $SkipAppBench) {
    throw "Cannot specify both -SkipCoreBench and -SkipAppBench; at least one workload is required."
}

$llvmProfdata = Find-LlvmProfdata
Write-Host "[pgo] llvm-profdata : $llvmProfdata" -ForegroundColor Cyan

if ([string]::IsNullOrWhiteSpace($Output)) {
    $Output = Join-Path $WorkDir "cellan.profdata"
}

$targetDir = Join-Path $WorkDir "target"
$tmpDir = Join-Path $WorkDir "tmp"
$rawDir = Join-Path $WorkDir "profraw"

if ($rawDir -match '\s') {
    throw "WorkDir must not contain whitespace (RUSTFLAGS -C profile-generate splits on spaces): $rawDir"
}

New-Item -ItemType Directory -Force -Path $targetDir, $tmpDir, $rawDir | Out-Null
$outParent = Split-Path -Parent $Output
if ($outParent) {
    New-Item -ItemType Directory -Force -Path $outParent | Out-Null
}

# 清理历史 .profraw，确保本次采集只包含当前源码版本的热路径计数
Get-ChildItem -LiteralPath $rawDir -Filter "*.profraw" -ErrorAction SilentlyContinue | Remove-Item -Force

$env:CARGO_TARGET_DIR = $targetDir
$env:TEMP = $tmpDir
$env:TMP = $tmpDir
$env:LLVM_PROFILE_FILE = Join-Path $rawDir "cellan-%p-%m.profraw"
$env:RUSTFLAGS = "-C target-feature=+crt-static -C target-cpu=x86-64-v2 -C profile-generate=$rawDir"

$commonCargoFlags = @("--release", "--locked")
if (-not $Online) {
    $commonCargoFlags += "--offline"
}

Write-Host "[pgo] WorkDir       : $WorkDir" -ForegroundColor Cyan
Write-Host "[pgo] TargetDir     : $targetDir" -ForegroundColor Cyan
Write-Host "[pgo] RawProfDir    : $rawDir" -ForegroundColor Cyan
Write-Host "[pgo] RUSTFLAGS     : $($env:RUSTFLAGS)" -ForegroundColor Cyan

if (-not $SkipCoreBench) {
    Write-Host ""
    Write-Host "[pgo] Step 1/3: Building & running sheetcore perf_wave_bench..." -ForegroundColor Yellow
    $coreArgs = @("run") + $commonCargoFlags + @("-p", "sheetcore", "--example", "perf_wave_bench")
    if (-not [string]::IsNullOrWhiteSpace($BenchFilter)) {
        $coreArgs += @("--", "--filter", $BenchFilter)
    }
    & cargo @coreArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Instrumented perf_wave_bench failed with exit code $LASTEXITCODE"
    }
}

if (-not $SkipAppBench) {
    Write-Host ""
    Write-Host "[pgo] Step 2/3: Building & running headless app perf_wave_app_suite..." -ForegroundColor Yellow
    $appArgs = @("test") + $commonCargoFlags + @(
        "-p", "app", "--no-default-features", "--features", "glow",
        "--bin", "app", "perf_wave_app_suite",
        "--", "--ignored", "--nocapture", "--test-threads=1"
    )
    & cargo @appArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Instrumented perf_wave_app_suite failed with exit code $LASTEXITCODE"
    }
}

Write-Host ""
Write-Host "[pgo] Step 3/3: Merging .profraw files with llvm-profdata..." -ForegroundColor Yellow
$rawFiles = @(Get-ChildItem -LiteralPath $rawDir -Filter "*.profraw" | Select-Object -ExpandProperty FullName)
if ($rawFiles.Count -eq 0) {
    throw "No .profraw files were generated in $rawDir"
}

& $llvmProfdata merge -o $Output @rawFiles
if ($LASTEXITCODE -ne 0) {
    throw "llvm-profdata merge failed with exit code $LASTEXITCODE"
}

$resolvedOut = (Resolve-Path -LiteralPath $Output).Path
$outSizeKb = [math]::Round((Get-Item -LiteralPath $resolvedOut).Length / 1KB, 1)

Write-Host ""
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host " PGO profile data generated successfully!" -ForegroundColor Green
Write-Host " Merged files : $($rawFiles.Count) .profraw -> $resolvedOut ($outSizeKb KB)"
Write-Host ""
Write-Host " Next step (build signed PGO release via publish.ps1):" -ForegroundColor Yellow
Write-Host "   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\publish.ps1 `"`" `"<更新说明>`" -PgoProfdata `"$resolvedOut`""
Write-Host "==========================================================" -ForegroundColor Cyan
