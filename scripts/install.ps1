<#
.SYNOPSIS
    Installs velm and the term-* companion binaries on Windows (x86_64).

.DESCRIPTION
    Downloads the latest (or specified) velm Windows x86_64 release archive from
    GitHub Releases, verifies its SHA-256 checksum, extracts the binaries into
    %LOCALAPPDATA%\velm\bin, and adds that folder to the user PATH.

.EXAMPLE
    irm https://raw.githubusercontent.com/Nokky21/velm/main/scripts/install.ps1 | iex

.EXAMPLE
    .\scripts\install.ps1 -Version v0.1.0 -InstallDir "$env:LOCALAPPDATA\velm\bin"
#>
[CmdletBinding()]
param(
    [string]$Version = $env:VELM_VERSION,
    [string]$InstallDir = $(if ($env:VELM_INSTALL_DIR) { $env:VELM_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "velm\bin" }),
    [switch]$NoPathUpdate
)

$ErrorActionPreference = "Stop"
$Repo = "Nokky21/velm"

if ([Environment]::Is64BitOperatingSystem -eq $false) {
    throw "velm Windows release binaries require 64-bit Windows (x86_64)."
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    Write-Host "velm: checking latest GitHub release..."
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "velm-installer" }
        $Version = $release.tag_name
    } catch {
        throw "No published GitHub release found yet for $Repo. Build from source with: cargo install --path . --locked"
    }
}

if (-not $Version.StartsWith("v")) {
    $Version = "v$Version"
}

$ArchiveName = "velm-$Version-windows-x86_64.zip"
$BaseUrl = "https://github.com/$Repo/releases/download/$Version"
$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("velm-install-" + [System.Guid]::NewGuid().ToString("N"))

New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    $ArchivePath = Join-Path $TmpDir $ArchiveName
    $ChecksumPath = Join-Path $TmpDir "$ArchiveName.sha256"

    Write-Host "velm: downloading $ArchiveName..."
    Invoke-WebRequest -Uri "$BaseUrl/$ArchiveName" -OutFile $ArchivePath -UseBasicParsing
    Invoke-WebRequest -Uri "$BaseUrl/$ArchiveName.sha256" -OutFile $ChecksumPath -UseBasicParsing

    Write-Host "velm: verifying SHA-256 checksum..."
    $ExpectedHash = ((Get-Content -Path $ChecksumPath -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
    $ActualHash = (Get-FileHash -Path $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ExpectedHash -ne $ActualHash) {
        throw "SHA-256 mismatch! expected=$ExpectedHash actual=$ActualHash"
    }

    $ExtractDir = Join-Path $TmpDir "extracted"
    Expand-Archive -Path $ArchivePath -DestinationPath $ExtractDir -Force

    $PayloadDir = Join-Path $ExtractDir "velm-$Version-windows-x86_64"
    if (-not (Test-Path $PayloadDir)) {
        $PayloadDir = $ExtractDir
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    foreach ($bin in @("velm.exe", "term-ai.exe", "term-repos.exe", "term-sql.exe", "term-claude.exe")) {
        $src = Join-Path $PayloadDir $bin
        if (Test-Path $src) {
            Copy-Item -Path $src -Destination (Join-Path $InstallDir $bin) -Force
        }
    }

    if (-not $NoPathUpdate) {
        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        $segments = if ($userPath) { $userPath -split ';' | Where-Object { $_ -ne '' } } else { @() }
        if ($segments -notcontains $InstallDir) {
            $newPath = ($segments + $InstallDir) -join ';'
            [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
            Write-Host "velm: added $InstallDir to your user PATH (restart your terminal to use it)."
        }
    }

    Write-Host ""
    Write-Host "velm $Version installed in $InstallDir"
    Write-Host "Next steps:"
    Write-Host "  1. Open a new Windows Terminal / PowerShell window"
    Write-Host "  2. Generate base config:  velm --default-config > `$env:APPDATA\velm\config.toml"
    Write-Host "  3. Launch velm:           velm"
} finally {
    if (Test-Path $TmpDir) {
        Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
