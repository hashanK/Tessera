# Builds the installer: dist\tessera-setup-<version>.exe
# The payload (tessera.exe, tessera-settings.exe) is built first, then embedded in the setup program.
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
cargo build --release -p tessera-setup
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force dist | Out-Null
$out = "dist\tessera-setup-$version.exe"
Copy-Item target\release\tessera-setup.exe $out -Force
Get-Item $out | Select-Object FullName, Length
