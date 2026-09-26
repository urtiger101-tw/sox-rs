#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Builds soundx release binary and creates platform installation packages.
.DESCRIPTION
    - Builds the project in release mode
    - Copies the binary to dist/
    - Creates platform-specific archive (zip for Windows, tar.gz for Unix)
    - Generates SHA256 checksums
#>

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Metadata = & cargo metadata --manifest-path (Join-Path $ProjectRoot 'Cargo.toml') --format-version=1 --no-deps
if ($LASTEXITCODE -ne 0) { throw 'Failed to read Cargo package metadata' }
$Version = $Metadata | ConvertFrom-Json | Select-Object -ExpandProperty packages | Where-Object name -eq "soundx" | Select-Object -ExpandProperty version

Write-Host "=== Packaging soundx v$Version ===" -ForegroundColor Cyan

# Step 1: Build release
Write-Host "`n[1/4] Building release binary..." -ForegroundColor Yellow
Push-Location $ProjectRoot
cargo build --release --locked
if ($LASTEXITCODE -ne 0) {
    throw "Release build failed"
}

# Step 2: Determine platform
$RustTarget = & rustc -vV | Select-String "host:" | ForEach-Object { $_ -replace "host: ", "" }
$RunningOnWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
$BinaryName = if ($RunningOnWindows) { "soundx.exe" } else { "soundx" }
$ArchiveName = "soundx-$Version-$RustTarget"

# Step 3: Prepare dist directory
Write-Host "[2/4] Preparing dist/$ArchiveName..." -ForegroundColor Yellow
$DistDir = Join-Path $ProjectRoot "dist"
$PackageDir = Join-Path $DistDir $ArchiveName
New-Item -ItemType Directory -Path $PackageDir -Force | Out-Null

# Copy binary
Copy-Item (Join-Path $ProjectRoot "target\release\$BinaryName") (Join-Path $PackageDir $BinaryName) -Force
Copy-Item (Join-Path $ProjectRoot "target\release\$BinaryName") (Join-Path $DistDir $BinaryName) -Force

# Copy README and license
Copy-Item (Join-Path $ProjectRoot "README.md") (Join-Path $PackageDir "README.md") -Force
Copy-Item (Join-Path $ProjectRoot "THIRD_PARTY_NOTICES.md") (Join-Path $PackageDir "THIRD_PARTY_NOTICES.md") -Force
Copy-Item (Join-Path $ProjectRoot "docs") $PackageDir -Recurse -Force
Copy-Item (Join-Path $ProjectRoot "pages") $PackageDir -Recurse -Force
if (Test-Path (Join-Path $ProjectRoot "LICENSE-MIT")) {
    Copy-Item (Join-Path $ProjectRoot "LICENSE-MIT") (Join-Path $PackageDir "LICENSE-MIT") -Force
}
if (Test-Path (Join-Path $ProjectRoot "LICENSE-LGPL")) {
    Copy-Item (Join-Path $ProjectRoot "LICENSE-LGPL") (Join-Path $PackageDir "LICENSE-LGPL") -Force
}

# Create examples directory
$ExamplesDir = Join-Path $PackageDir "examples"
New-Item -ItemType Directory -Path $ExamplesDir -Force | Out-Null
Copy-Item (Join-Path $ProjectRoot "examples\*") $ExamplesDir -Force

# Step 4: Create archive
Write-Host "[3/4] Creating archive..." -ForegroundColor Yellow
if ($RunningOnWindows) {
    $ArchiveFile = "$ArchiveName.zip"
    Compress-Archive -Path "$PackageDir\*" -DestinationPath (Join-Path $DistDir $ArchiveFile) -Force
} else {
    $ArchiveFile = "$ArchiveName.tar.gz"
    & tar -czf (Join-Path $DistDir $ArchiveFile) -C $DistDir $ArchiveName
    if ($LASTEXITCODE -ne 0) { throw 'Archive creation failed' }
}

# Step 5: Generate checksums
Write-Host "[4/4] Generating checksums..." -ForegroundColor Yellow
$ChecksumFile = Join-Path $DistDir "SHA256SUMS.txt"
if (Test-Path $ChecksumFile) { Remove-Item -LiteralPath $ChecksumFile }
if ($RunningOnWindows) {
    $Iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 7\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($Iscc) {
        & $Iscc "/DAppVersion=$Version" (Join-Path $ProjectRoot "installer\soundx.iss")
        if ($LASTEXITCODE -ne 0) { throw "Installer build failed" }
    } else {
        Write-Warning "Inno Setup not found; skipping setup.exe"
    }
}
Get-ChildItem -Path $DistDir -File | Where-Object Name -Match '^soundx(?:-.*)?\.(zip|tar\.gz|exe)$' | ForEach-Object {
    $hash = Get-FileHash $_.FullName -Algorithm SHA256
    "$($hash.Hash.ToLower())  $($_.Name)" | Out-File -FilePath $ChecksumFile -Append -Encoding ascii
}

Write-Host "`n=== Package complete ===" -ForegroundColor Green
Write-Host "Binary: target/release/$BinaryName"
Write-Host "Package: dist/$ArchiveFile"
Write-Host "Checksums: dist/SHA256SUMS.txt"
Pop-Location
