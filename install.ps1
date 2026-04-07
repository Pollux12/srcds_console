# srcds_console installer
# Usage: irm https://raw.githubusercontent.com/Pollux12/srcds_console/master/install.ps1 | iex

$ErrorActionPreference = 'Stop'
$repo = 'Pollux12/srcds_console'

# Must be in the SRCDS game root
if (-not (Test-Path 'srcds.exe')) {
    Write-Host "Error: srcds.exe not found in current directory." -ForegroundColor Red
    Write-Host "Navigate to your SRCDS game root first (the folder containing srcds.exe)."
    return
}

# Auto-detect architecture
if (Test-Path 'srcds_win64.exe') {
    $asset = 'srcds_win64_console.exe'
    Write-Host "Detected x64 branch (srcds_win64.exe found)" -ForegroundColor Cyan
} else {
    $asset = 'srcds_console.exe'
    Write-Host "Detected x86 branch" -ForegroundColor Cyan
}

# Get latest release download URL
Write-Host "Fetching latest release from $repo..." -ForegroundColor Gray
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$url = ($release.assets | Where-Object { $_.name -eq $asset }).browser_download_url

if (-not $url) {
    Write-Host "Error: Asset '$asset' not found in latest release ($($release.tag_name))." -ForegroundColor Red
    return
}

Write-Host "Downloading $asset ($($release.tag_name))..." -ForegroundColor Gray
Invoke-WebRequest -Uri $url -OutFile $asset -UseBasicParsing

Write-Host ""
Write-Host "Installed $asset ($($release.tag_name)) to $(Get-Location)" -ForegroundColor Green
Write-Host "Run it with: ./$asset +maxplayers 20 -console +gamemode sandbox -port 27015 +map gm_construct -tickrate 22"
