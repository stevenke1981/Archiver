# Silent per-user install / verify / uninstall of the Inno Setup installer.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$setup = Join-Path $root 'dist\archiver-setup-windows-x64.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Archiver'
function Check($cond, $msg) { if (-not $cond) { throw "FAILED: $msg" } ; Write-Host "ok: $msg" }

$p = Start-Process $setup -Wait -PassThru -ArgumentList @(
    '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER',
    '/TASKS=desktopicon,assoc,contextmenu,addtopath', '/LOG=install.log')
Check ($p.ExitCode -eq 0) "installer exit code 0 (got $($p.ExitCode))"

foreach ($f in 'archiver.exe', 'rarc.exe', 'README.md', 'LICENSE.txt', 'unins000.exe') {
    Check (Test-Path (Join-Path $dir $f)) "installed $f"
}
Check (Test-Path 'HKCU:\Software\Classes\Archiver.archive\shell\open\command') "ProgID Archiver.archive"
Check ((Get-ItemProperty 'HKCU:\Software\Classes\.7z\OpenWithProgids').PSObject.Properties.Name -contains 'Archiver.archive') ".7z OpenWithProgids"
Check (Test-Path 'HKCU:\Software\Classes\Directory\shell\Archiver') "Explorer context menu registered"
Check (Test-Path 'HKCU:\Software\Classes\SystemFileAssociations\.zip\shell\Archiver') "zip context menu registered"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
Check ($userPath -like "*$dir*") "install dir on user PATH"
Check (Test-Path (Join-Path ([Environment]::GetFolderPath('Programs')) 'Archiver.lnk')) "Start menu shortcut"
Check (Test-Path (Join-Path ([Environment]::GetFolderPath('Desktop')) 'Archiver.lnk')) "desktop shortcut"

$ver = & (Join-Path $dir 'rarc.exe') --version
Check ($LASTEXITCODE -eq 0) "rarc --version runs: $ver"
# Round trip through the installed CLI
$tmp = Join-Path $env:RUNNER_TEMP 'smoke'
New-Item -ItemType Directory -Force "$tmp\in" | Out-Null
Set-Content "$tmp\in\hello.txt" 'hello from installer smoke test'
& (Join-Path $dir 'rarc.exe') c "$tmp\t.7z" "$tmp\in"; Check ($LASTEXITCODE -eq 0) "rarc create 7z"
& (Join-Path $dir 'rarc.exe') l "$tmp\t.7z"; Check ($LASTEXITCODE -eq 0) "rarc list 7z"

$p = Start-Process (Join-Path $dir 'unins000.exe') -Wait -PassThru -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
Check ($p.ExitCode -eq 0) "uninstaller exit code 0"
Start-Sleep -Seconds 3   # uninstaller deletes itself via a helper process
Check (-not (Test-Path (Join-Path $dir 'archiver.exe'))) "archiver.exe removed"
Check (-not (Test-Path 'HKCU:\Software\Classes\Archiver.archive')) "ProgID removed"
Check (-not (Test-Path 'HKCU:\Software\Classes\Directory\shell\Archiver')) "context menu removed"
$po = Get-ItemProperty 'HKCU:\Software\Classes\.7z\OpenWithProgids' -ErrorAction SilentlyContinue
Check (-not ($po -and ($po.PSObject.Properties.Name -contains 'Archiver.archive'))) ".7z OpenWithProgids entry removed"
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
Check (-not ($userPath -like "*$dir*")) "removed from user PATH"
Check (-not (Test-Path (Join-Path ([Environment]::GetFolderPath('Programs')) 'Archiver.lnk'))) "Start menu shortcut removed"
Write-Host "Installer smoke test passed"
