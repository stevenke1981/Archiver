# Launch the GUI and capture the screen so rendering can be checked from CI artifacts.
param([string]$Exe = 'target\release\downloader.exe', [string]$Out = 'win-screenshot.png')
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$env:DOWNLOADER_STATE_DIR = Join-Path $env:RUNNER_TEMP 'dlstate'
New-Item -ItemType Directory -Force $env:DOWNLOADER_STATE_DIR | Out-Null
Set-Content (Join-Path $env:DOWNLOADER_STATE_DIR 'settings.json') '{"language":"zh_TW"}' -Encoding utf8
$p = Start-Process $Exe -PassThru
Start-Sleep -Seconds 10
if ($p.HasExited) { throw "GUI exited early with code $($p.ExitCode)" }
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Stop-Process -Id $p.Id -Force
Write-Host "saved $Out ($($b.Width)x$($b.Height))"
