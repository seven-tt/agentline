# Agentline Installer for Windows
# Usage: irm https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.ps1 | iex

$ErrorActionPreference = "Stop"

$RepoUrl = "https://github.com/seven-tt/agentline"

# ── check Rust ───────────────────────────────────────────────────
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "Rust toolchain not found." -ForegroundColor Red
    Write-Host "Install it first: https://rustup.rs" -ForegroundColor Yellow
    exit 1
}

$rustVersion = (rustc --version) -replace 'rustc\s+', '' -replace '\s.*', ''
Write-Host "Rust $rustVersion detected" -ForegroundColor Green

# ── get source ───────────────────────────────────────────────────
if ((Test-Path "Cargo.toml") -and (Test-Path "crates")) {
    $sourceDir = (Get-Location).Path
    Write-Host "Using current directory as source"
} else {
    $sourceDir = Join-Path ([System.IO.Path]::GetTempPath()) "agentline-install"
    if (Test-Path $sourceDir) { Remove-Item -Recurse -Force $sourceDir }
    Write-Host "Cloning $RepoUrl ..."
    git clone --depth 1 "$RepoUrl.git" $sourceDir
    Set-Location $sourceDir
}

# ── build ────────────────────────────────────────────────────────
Write-Host "Building agentline ..." -ForegroundColor Cyan
cargo build --release --package agentline

Write-Host "Building agentline-tray ..." -ForegroundColor Cyan
cargo build --release --package agentline-tray

# ── install binaries ─────────────────────────────────────────────
$installDir = Join-Path $env:LOCALAPPDATA "agentline\bin"
if (-not (Test-Path $installDir)) { New-Item -ItemType Directory -Path $installDir -Force | Out-Null }

Copy-Item "target\release\agentline.exe" (Join-Path $installDir "agentline.exe") -Force
Copy-Item "target\release\agentline-tray.exe" (Join-Path $installDir "agentline-tray.exe") -Force

# add to user PATH if needed
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$installDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$installDir", "User")
    Write-Host "Added $installDir to user PATH" -ForegroundColor Yellow
    Write-Host "  Restart your terminal for PATH changes to take effect."
}

Write-Host "Installed agentline -> $installDir\agentline.exe" -ForegroundColor Green
Write-Host "Installed agentline-tray -> $installDir\agentline-tray.exe" -ForegroundColor Green

# ── init config ──────────────────────────────────────────────────
$configDir = Join-Path $env:USERPROFILE ".agentline"
$configFile = Join-Path $configDir "config.toml"

if (-not (Test-Path $configFile)) {
    if (-not (Test-Path $configDir)) { New-Item -ItemType Directory -Path $configDir -Force | Out-Null }
    Copy-Item (Join-Path $sourceDir "config.example.toml") $configFile
    Write-Host "Created default config at $configFile" -ForegroundColor Green
    Write-Host "  Edit it to set IM credentials and agent backend, then run 'agentline'."
} else {
    Write-Host "Config already exists at $configFile"
}

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host "  Binary: $installDir\agentline.exe"
Write-Host "  Tray:   $installDir\agentline-tray.exe"
Write-Host "  Config: $configFile"
