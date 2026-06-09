# Agentline Installer for Windows
# Usage: irm https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.ps1 | iex

Write-Host "❌ Windows is not yet supported by Agentline." -ForegroundColor Red
Write-Host ""
Write-Host "Recommended alternatives:" -ForegroundColor Yellow
Write-Host "  1. Use Windows Subsystem for Linux (WSL) and run the Linux installer."
Write-Host "  2. Build from source with cargo (requires Rust + MSVC toolchain)."
Write-Host ""
Write-Host "Linux install command (run inside WSL):" -ForegroundColor Cyan
Write-Host '     curl -fsSL https://raw.githubusercontent.com/seven-tt/agentline/main/scripts/install.sh | bash'
