# Builds dist\archiver-setup-windows-x64.exe with Inno Setup.
# Expects target\release\archiver.exe and rarc.exe to exist already.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
if (-not (Test-Path $iscc)) {
    choco install innosetup -y --no-progress
    if ($LASTEXITCODE -ne 0) { throw "choco install innosetup failed" }
}
if (-not (Test-Path $iscc)) { throw "ISCC.exe not found at $iscc" }

$isl = 'installer\ChineseTraditional.isl'
if (-not (Test-Path $isl)) {
    Invoke-WebRequest -UseBasicParsing -OutFile $isl `
        'https://raw.githubusercontent.com/jrsoftware/issrc/main/Files/Languages/Unofficial/ChineseTraditional.isl'
}

$m = Select-String -Path 'gui\Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
$version = $m.Matches[0].Groups[1].Value
Write-Host "Building installer for Archiver $version"

New-Item -ItemType Directory -Force dist | Out-Null
& $iscc /Qp "/DAppVersion=$version" "/DSrcDir=$root\target\release" 'installer\archiver.iss'
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }
Get-Item 'dist\archiver-setup-windows-x64.exe' | Format-List Name, Length
