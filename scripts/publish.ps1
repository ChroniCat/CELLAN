param(
    [Parameter(Position = 0)]
    [string]$Version = "",
    [Parameter(Position = 1)]
    [string]$Notes = "",
    [string]$NotesFile = "",
    [int]$Port = 48768,
    [string]$UpdateUrl = "",
    [switch]$NoServe,
    [switch]$PrepareOnly
)

$ErrorActionPreference = "Stop"
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Read-Utf8Text([string]$Path) {
    return [System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8)
}

function Write-Utf8Text([string]$Path, [string]$Content) {
    if ([System.IO.File]::Exists($Path) -and (Read-Utf8Text $Path) -ceq $Content) { return }
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
    [System.IO.File]::WriteAllText($Path, $Content, $utf8NoBom)
}

function Normalize-Notes([string]$Raw) {
    if ([string]::IsNullOrWhiteSpace($Raw)) {
        return ""
    }
    $sep = [string][char]0xFF1B
    $lines = ($Raw -replace "`r`n", "`n" -replace "`r", "`n") -split "`n" |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_.Length -gt 0 }
    $joined = [string]::Join($sep, $lines)
    $sb = New-Object System.Text.StringBuilder
    foreach ($ch in $joined.ToCharArray()) {
        if (-not [char]::IsControl($ch)) {
            [void]$sb.Append($ch)
            if ($sb.Length -ge 500) { break }
        }
    }
    return $sb.ToString()
}

function Get-PrimaryLanIPv4 {
    $route = Get-NetRoute -DestinationPrefix "0.0.0.0/0" -ErrorAction SilentlyContinue |
        Sort-Object { $_.RouteMetric + $_.InterfaceMetric } |
        Select-Object -First 1
    if ($route) {
        $ip = Get-NetIPAddress -InterfaceIndex $route.InterfaceIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue |
            Where-Object { $_.IPAddress -notlike "169.254.*" -and $_.IPAddress -ne "127.0.0.1" } |
            Select-Object -First 1 -ExpandProperty IPAddress
        if ($ip) { return $ip }
    }
    $fallback = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object {
            $_.IPAddress -ne "127.0.0.1" -and
            $_.IPAddress -notlike "169.254.*" -and
            $_.InterfaceAlias -notmatch "WSL|vEthernet|Loopback|VMware|VirtualBox|Docker"
        } |
        Select-Object -First 1 -ExpandProperty IPAddress
    return $fallback
}

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $cargoBin) -and ($env:PATH -notlike "*$cargoBin*")) {
    $env:PATH = "$cargoBin;$env:PATH"
}

$updateRsPath = Join-Path $repoRoot "crates\app\src\update.rs"
$updateRs = Read-Utf8Text $updateRsPath

if ($updateRs -notmatch 'pub const VERSION:\s*&str\s*=\s*"([0-9]+\.[0-9]+(?:\.[0-9]+)?)"') {
    throw "Cannot locate VERSION constant in crates/app/src/update.rs"
}
$currentVer = $Matches[1]
if ($updateRs -notmatch 'pub const DEFAULT_PUBKEY_HEX:\s*&str\s*=\s*"([^"]*)"') {
    throw "Cannot locate DEFAULT_PUBKEY_HEX in crates/app/src/update.rs"
}
$pinnedPubHex = $Matches[1]

if ([string]::IsNullOrWhiteSpace($Version)) {
    $segs = $currentVer.Split('.')
    $maj = [int]$segs[0]
    $min = [int]$segs[1] + 1
    $shortVer = "$maj.$min"
} else {
    $cleanV = $Version.Trim().TrimStart('v', 'V')
    if ($cleanV -notmatch '^[0-9]+\.[0-9]+(?:\.[0-9]+)?$') {
        throw "Invalid version '$Version' (expected e.g. 1.71)"
    }
    $segs = $cleanV.Split('.')
    $shortVer = "$($segs[0]).$($segs[1])"
}
$cargoVer = if ($shortVer.Split('.').Count -eq 2) { "$shortVer.0" } else { $shortVer }

# 1. Ensure ECDSA P-256 release signing keypair in %USERPROFILE%\.cellan-release
$keyDir = Join-Path $env:USERPROFILE ".cellan-release"
$privKeyPath = Join-Path $keyDir "signing-key.blob"
if (-not (Test-Path $keyDir)) {
    New-Item -ItemType Directory -Path $keyDir -Force | Out-Null
}

if (-not (Test-Path $privKeyPath)) {
    if ($pinnedPubHex) {
        throw "The official signing key is missing. Restore the existing key to $privKeyPath."
    }
    if (-not $PrepareOnly) {
        throw "Initialize the signing key with -PrepareOnly and commit the public key before building."
    }
    $generated = New-Object System.Security.Cryptography.ECDsaCng(256)
    $candidatePath = Join-Path $keyDir ([Guid]::NewGuid().ToString() + ".tmp")
    try {
        [System.IO.File]::WriteAllBytes($candidatePath, $generated.Key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob))
        [System.IO.File]::Move($candidatePath, $privKeyPath)
    } finally {
        $generated.Dispose()
        [System.IO.File]::Delete($candidatePath)
    }
    Write-Host "[key] Generated new ECDSA P-256 release signing key: $privKeyPath" -ForegroundColor Green
}
$privBlob = [System.IO.File]::ReadAllBytes($privKeyPath)
$cngKey = [System.Security.Cryptography.CngKey]::Import(
    $privBlob,
    [System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob
)
$ec = New-Object System.Security.Cryptography.ECDsaCng($cngKey)
$ec.HashAlgorithm = [System.Security.Cryptography.CngAlgorithm]::Sha256

$pubBlob = $ec.Key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
if ($pubBlob.Length -ne 72) {
    throw "Unexpected EccPublicBlob length: $($pubBlob.Length)"
}
$pubHex = ([BitConverter]::ToString($pubBlob, 8, 64).Replace("-", "")).ToLowerInvariant()
if ($pinnedPubHex -and $pinnedPubHex -ne $pubHex) {
    throw "The local signing key does not match the official public key recorded in this checkout."
}

# 2. Resolve intranet HTTP update URL automatically from current machine IP
if (-not ($PrepareOnly -or $NoServe) -and [string]::IsNullOrWhiteSpace($UpdateUrl)) {
    $lanIp = Get-PrimaryLanIPv4
    if ([string]::IsNullOrWhiteSpace($lanIp)) {
        throw "Could not detect primary LAN IPv4 automatically; pass -UpdateUrl http://<ip>:$Port"
    }
    $UpdateUrl = "http://${lanIp}:${Port}"
}
$UpdateUrl = $UpdateUrl.Trim().TrimEnd('/')

Write-Host "[publish] Version: v$shortVer (Cargo $cargoVer)" -ForegroundColor Cyan
Write-Host "[publish] Default Update URL: $UpdateUrl" -ForegroundColor Cyan

# 3. Synchronize version numbers & embedded public key across workspace
$updateRs = [regex]::Replace(
    $updateRs,
    'pub const VERSION:\s*&str\s*=\s*"[^"]*";',
    "pub const VERSION: &str = `"$shortVer`";"
)
$updateRs = [regex]::Replace(
    $updateRs,
    'pub const DEFAULT_PUBKEY_HEX:\s*&str\s*=\s*"[^"]*";',
    "pub const DEFAULT_PUBKEY_HEX: &str = `"$pubHex`";"
)
Write-Utf8Text $updateRsPath $updateRs

foreach ($relToml in @("Cargo.toml", "crates\core\Cargo.toml", "crates\app\Cargo.toml")) {
    $p = Join-Path $repoRoot $relToml
    $txt = Read-Utf8Text $p
    $txt = [regex]::Replace($txt, '(?m)^version\s*=\s*"[0-9]+\.[0-9]+\.[0-9]+"', "version = `"$cargoVer`"")
    $txt = [regex]::Replace($txt, 'sheetcore\s*=\s*\{\s*version\s*=\s*"[0-9]+\.[0-9]+\.[0-9]+"', "sheetcore = { version = `"$cargoVer`"")
    Write-Utf8Text $p $txt
}

$manualPath = Join-Path $repoRoot "packaging\$([char]0x4F7F)$([char]0x7528)$([char]0x8BF4)$([char]0x660E).txt"
if (Test-Path $manualPath) {
    $mTxt = Read-Utf8Text $manualPath
    $verPrefix = "$([char]0x7248)$([char]0x672C)$([char]0xFF1A)"
    $mTxt = [regex]::Replace($mTxt, "(?m)^$([regex]::Escape($verPrefix))[^\r\n]*", "${verPrefix}${shortVer}")
    Write-Utf8Text $manualPath $mTxt
}

$lockPath = Join-Path $repoRoot "Cargo.lock"
$lock = Read-Utf8Text $lockPath
$lock = [regex]::Replace($lock, '(?m)^(name = "(?:app|sheetcore)"\r?\nversion = ")[^"]+(")', ('${1}' + $cargoVer + '$2'))
Write-Utf8Text $lockPath $lock
if ($PrepareOnly) {
    $ec.Dispose()
    $cngKey.Dispose()
    Write-Host "[prepare] Version and public key are ready for review and commit."
    return
}

$releaseCommit = git rev-parse --verify HEAD
if ($LASTEXITCODE -ne 0 -or $releaseCommit -notmatch '^[0-9a-f]{40,64}$') {
    throw "Cannot determine the release Git commit."
}
$pending = git status --porcelain
if ($LASTEXITCODE -ne 0 -or $pending) {
    throw "Release sources must be clean. Review and commit the prepared version and public key before building."
}

# 4. Build static single-file release executable with embedded update endpoint
$env:CELLAN_RELEASE_COMMIT = $releaseCommit
$env:CARGO_TARGET_DIR = Join-Path $repoRoot "target"
$env:RUSTFLAGS = "-C target-feature=+crt-static -C target-cpu=x86-64-v2"
$env:CELLAN_DEFAULT_UPDATE_URL = $UpdateUrl
$env:CELLAN_UPDATE_PUBKEY = $pubHex

Write-Host "[build] Building static release CELLAN.exe (wgpu + glow)..." -ForegroundColor Yellow
& cargo build --release --locked --offline -p app --features wgpu,glow
if ($LASTEXITCODE -ne 0) {
    if ($NoServe) { throw "Offline cargo build failed with exit code $LASTEXITCODE" }
    Write-Host "[build] Offline cache incomplete, retrying cargo build without --offline..." -ForegroundColor Yellow
    & cargo build --release --locked -p app --features wgpu,glow
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}

$builtExe = Join-Path $repoRoot "target\release\app.exe"
if (-not (Test-Path $builtExe)) {
    throw "Release binary not found at $builtExe"
}

# 5. Compute SHA-256, ECDSA P-256 signature, manifest.txt, and append CLNUPD01 PE overlay trailer
$rawBytes = [System.IO.File]::ReadAllBytes($builtExe)
$size = [uint64]$rawBytes.LongLength
$sha256Obj = [System.Security.Cryptography.SHA256]::Create()
$shaHex = ([BitConverter]::ToString($sha256Obj.ComputeHash($rawBytes)).Replace("-", "")).ToLowerInvariant()
$sha256Obj.Dispose()

if (-not [string]::IsNullOrWhiteSpace($NotesFile)) {
    $Notes = Read-Utf8Text $NotesFile
}
$cleanNotes = Normalize-Notes $Notes
$canonical = "CELLAN-RELEASE-V1`nversion=$shortVer`nsize=$size`nsha256=$shaHex`nnotes=$cleanNotes`n"
$canonicalBytes = $utf8NoBom.GetBytes($canonical)
$sigBytes = $ec.SignData($canonicalBytes)
$ec.Dispose()
if ($sigBytes.Length -ne 64) {
    throw "Unexpected ECDSA signature length: $($sigBytes.Length)"
}
$sigHex = ([BitConverter]::ToString($sigBytes).Replace("-", "")).ToLowerInvariant()

$manifestText = "version=$shortVer`nexe=CELLAN.exe`nsize=$size`nsha256=$shaHex`nsig=$sigHex`nnotes=$cleanNotes`n"
$manifestBytes = $utf8NoBom.GetBytes($manifestText)
$lenBytes = [BitConverter]::GetBytes([uint32]$manifestBytes.Length)
$magicBytes = [System.Text.Encoding]::ASCII.GetBytes("CLNUPD01")

$distDir = Join-Path $repoRoot "dist\CELLAN"
New-Item -ItemType Directory -Path $distDir -Force | Out-Null
$distExe = Join-Path $distDir "CELLAN.exe"

$fs = [System.IO.File]::Open($distExe, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write)
try {
    $fs.Write($rawBytes, 0, $rawBytes.Length)
    $fs.Write($manifestBytes, 0, $manifestBytes.Length)
    $fs.Write($lenBytes, 0, $lenBytes.Length)
    $fs.Write($magicBytes, 0, $magicBytes.Length)
    $fs.Flush()
} finally {
    $fs.Dispose()
}

Write-Utf8Text (Join-Path $distDir "manifest.txt") $manifestText
$verTxtName = "$([char]0x7248)$([char]0x672C).txt"
Write-Utf8Text (Join-Path $distDir $verTxtName) "CELLAN v$shortVer`nGit: $releaseCommit`nSHA256: $shaHex`nUpdateURL: $UpdateUrl`n"

$noticesSrc = Join-Path $repoRoot "packaging\THIRD_PARTY_NOTICES.txt"
if (Test-Path $noticesSrc) {
    Copy-Item $noticesSrc (Join-Path $distDir "THIRD_PARTY_NOTICES.txt") -Force
}
if (Test-Path $manualPath) {
    Copy-Item $manualPath (Join-Path $distDir (Split-Path -Leaf $manualPath)) -Force
}

Write-Host "[sign] Signed CELLAN.exe ($('{0:N1}' -f ($size / 1MB)) MB, SHA256=$($shaHex.Substring(0, 12))...)" -ForegroundColor Green

# 6. Publish to local silent HTTP server & auto-start on login
if (-not $NoServe) {
    $appDataRoot = Join-Path $env:LOCALAPPDATA "CELLAN"
    $serveDir = Join-Path $appDataRoot "publish-server"
    $hostDir = Join-Path $appDataRoot "publish-host"
    New-Item -ItemType Directory -Path $serveDir -Force | Out-Null
    New-Item -ItemType Directory -Path $hostDir -Force | Out-Null

    $tmpServeExe = Join-Path $serveDir "CELLAN.exe.tmp"
    Copy-Item $distExe $tmpServeExe -Force
    Move-Item $tmpServeExe (Join-Path $serveDir "CELLAN.exe") -Force
    Write-Utf8Text (Join-Path $serveDir "manifest.txt") $manifestText

    # Ensure inbound firewall rule when running elevated so LAN peers can connect without block
    try {
        $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
        if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
            $ruleName = "CELLAN Update Server (TCP $Port)"
            $existing = Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue
            if (-not $existing) {
                New-NetFirewallRule -DisplayName $ruleName -Direction Inbound -Protocol TCP -LocalPort $Port -Action Allow -Profile Private,Domain | Out-Null
                Write-Host "[firewall] Allowed inbound TCP $Port on Private/Domain networks" -ForegroundColor Green
            }
        }
    } catch {
        # Non-fatal if firewall cmdlets unavailable
    }

    $serverBin = Join-Path $hostDir "cellan-update-server.exe"
    $alive = $false
    try {
        $resp = Invoke-WebRequest -Uri "http://127.0.0.1:${Port}/healthz" -UseBasicParsing -TimeoutSec 2 -ErrorAction Stop
        if ($resp.Content -match "CELLAN-UPDATE-OK") {
            $alive = $true
        }
    } catch {
        $alive = $false
    }

    if (-not $alive) {
        Copy-Item $distExe $serverBin -Force
        Start-Process -FilePath $serverBin -ArgumentList @("--serve-updates=$serveDir", "--port=$Port") -WindowStyle Hidden
        Start-Sleep -Milliseconds 450
    }

    # Register HKCU Run so reboots keep the silent update server alive automatically
    try {
        $runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
        $runCmd = "`"$serverBin`" `"--serve-updates=$serveDir`" --port=$Port"
        Set-ItemProperty -Path $runKey -Name "CELLANUpdateServer" -Value $runCmd -ErrorAction SilentlyContinue
    } catch {}

    $verifyResp = Invoke-WebRequest -Uri "http://127.0.0.1:${Port}/manifest.txt" -UseBasicParsing -TimeoutSec 5
    if ($verifyResp.StatusCode -ne 200 -or $verifyResp.Content -notmatch "version=$([regex]::Escape($shortVer))") {
        throw "Local update server verification failed on port $Port"
    }
    Write-Host "[serve] Live at $UpdateUrl/manifest.txt (silent background daemon active)" -ForegroundColor Green
}

Write-Host ""
Write-Host "==========================================================" -ForegroundColor Cyan
if ($NoServe) {
    Write-Host " Packaged CELLAN v$shortVer locally." -ForegroundColor Green
} else {
    Write-Host " Published CELLAN v$shortVer successfully!" -ForegroundColor Green
}
Write-Host " Dist package : $distExe"
Write-Host " Git commit   : $releaseCommit"
if (-not $NoServe) { Write-Host " Update URL   : $UpdateUrl" }
Write-Host "==========================================================" -ForegroundColor Cyan
