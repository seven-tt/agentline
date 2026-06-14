# Agentline Installer for Windows
# Usage: irm https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.ps1 | iex

$ErrorActionPreference = "Stop"

$Repo = "seven-tt/agentline"
$RepoUrl = "https://github.com/$Repo"
$Label = "win-x64"

# ── try downloading prebuilt installer from GitHub Releases ──────
$needBuild = $false

try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -ErrorAction Stop
    $tag = $release.tag_name
    $version = $tag.TrimStart("v")
    $setupName = "agentline-tray-$version-$Label-setup.exe"
    $url = "$RepoUrl/releases/download/$tag/$setupName"
    $tmpSetup = Join-Path ([System.IO.Path]::GetTempPath()) $setupName

    Write-Host "Downloading $setupName ..." -ForegroundColor Cyan
    Invoke-WebRequest -Uri $url -OutFile $tmpSetup -ErrorAction Stop
    Write-Host "Download complete. Launching installer ..." -ForegroundColor Green
    Start-Process -FilePath $tmpSetup -Wait
    Remove-Item -Path $tmpSetup -ErrorAction SilentlyContinue
    Write-Host "Installer finished." -ForegroundColor Green
} catch {
    Write-Host "Download failed, will try building from source." -ForegroundColor Yellow
    $needBuild = $true
}

# ── fallback: build from source ──────────────────────────────────
if ($needBuild) {
    Write-Host ""
    Write-Host "Falling back to building from source ..." -ForegroundColor Yellow

    $installDir = Join-Path $env:LOCALAPPDATA "agentline\bin"
    if (-not (Test-Path $installDir)) { New-Item -ItemType Directory -Path $installDir -Force | Out-Null }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host "Rust toolchain not found." -ForegroundColor Red
        Write-Host "Install it first: https://rustup.rs" -ForegroundColor Yellow
        exit 1
    }

    $rustVersion = (rustc --version) -replace 'rustc\s+', '' -replace '\s.*', ''
    Write-Host "Rust $rustVersion detected" -ForegroundColor Green

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

    $cliBin = Join-Path $installDir "agentline.exe"
    if (-not (Test-Path $cliBin)) {
        Write-Host "Building agentline ..." -ForegroundColor Cyan
        cargo build --release --package agentline
        Copy-Item "target\release\agentline.exe" $cliBin -Force
        Write-Host "Installed agentline -> $cliBin" -ForegroundColor Green
    }

    $trayBin = Join-Path $installDir "agentline-tray.exe"
    if (-not (Test-Path $trayBin)) {
        Write-Host "Building agentline-tray ..." -ForegroundColor Cyan
        cargo build --release --package agentline-tray
        Copy-Item "target\release\agentline-tray.exe" $trayBin -Force
        Write-Host "Installed agentline-tray -> $trayBin" -ForegroundColor Green
    }

    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$installDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$userPath;$installDir", "User")
        Write-Host "Added $installDir to user PATH" -ForegroundColor Yellow
        Write-Host "  Restart your terminal for PATH changes to take effect."
    }
}

# ── init config ──────────────────────────────────────────────────
$configDir = Join-Path $env:USERPROFILE ".agentline"
$configFile = Join-Path $configDir "config.toml"

if (-not (Test-Path $configFile)) {
    if (-not (Test-Path $configDir)) { New-Item -ItemType Directory -Path $configDir -Force | Out-Null }
    $exampleConfig = $null
    if (Test-Path "config.example.toml") {
        $exampleConfig = "config.example.toml"
    } elseif ($sourceDir -and (Test-Path (Join-Path $sourceDir "config.example.toml"))) {
        $exampleConfig = Join-Path $sourceDir "config.example.toml"
    }
    if ($exampleConfig) {
        Copy-Item $exampleConfig $configFile
        Write-Host "Created default config at $configFile" -ForegroundColor Green
        Write-Host "  Edit it to set IM credentials and agent backend, then run 'agentline'."
    }
} else {
    Write-Host "Config already exists at $configFile"
}

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host "  Config: $configFile"
Write-Host "  Edit config, then run 'agentline' to start."
