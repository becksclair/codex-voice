#Requires -Version 5.1
<#
.SYNOPSIS
    Build and stage a Windows x64 distribution of Codex Voice.

.DESCRIPTION
    Run from the repository root on Windows. This script:
      1. Builds the web frontend (web/dist) so it is embedded in the binary.
      2. Builds the release binary and NSIS installer with Tauri 2.
      3. Stages dist/codex-voice-windows-x64/ with the exe plus a generated
         README.txt and install-autostart.ps1 helper.
      4. Zips the staging directory to dist/codex-voice-windows-x64.zip.
      5. Copies the NSIS installer to a stable dist path.
      6. Prints both paths and SHA256 values.

    The app's tray/window shell is Tauri 2, which renders its windows with
    WebView2 on Windows. The NSIS installer downloads the Evergreen bootstrapper
    when the runtime is missing. Portable-ZIP users should use the installer if
    their machine does not already have WebView2.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File packaging\windows\build-dist.ps1
#>

$ErrorActionPreference = "Stop"

# Resolve the repository root from this script's location so the build works
# regardless of the caller's current directory.
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Resolve-Path (Join-Path $scriptDir "..\..")
Set-Location $repoRoot

Write-Host "Repository root: $repoRoot"

$distName = "codex-voice-windows-x64"
$distDir = Join-Path $repoRoot "dist"
$stageDir = Join-Path $distDir $distName
$zipPath = Join-Path $distDir "$distName.zip"
$installerPath = Join-Path $distDir "$distName-setup.exe"

# --- 1. Build web frontend -------------------------------------------------
# web/dist is embedded into the binary by the transcriber crate's build.rs.
# Build it first so the packaged exe ships the real PWA and not the stub page.
$webDir = Join-Path $repoRoot "web"
if (-not (Get-Command bun -ErrorAction SilentlyContinue)) {
    throw "bun not found on PATH. Install bun to build the web frontend: scoop install bun"
}

Write-Host "Building web frontend (bun install --frozen-lockfile; bun run build)..."
Push-Location $webDir
try {
    bun install --frozen-lockfile
    if ($LASTEXITCODE -ne 0) {
        throw "bun install failed with exit code $LASTEXITCODE"
    }
    bun run build
    if ($LASTEXITCODE -ne 0) {
        throw "bun run build failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

# --- 2. Build binary + NSIS installer -------------------------------------
if (-not (Get-Command cargo-tauri -ErrorAction SilentlyContinue)) {
    throw "cargo-tauri not found on PATH. Install tauri-cli 2.11.4: cargo install tauri-cli --version 2.11.4 --locked"
}
Write-Host "Building release binary and NSIS installer (cargo tauri build --bundles nsis)..."
cargo tauri build --bundles nsis
if ($LASTEXITCODE -ne 0) {
    throw "cargo tauri build failed with exit code $LASTEXITCODE"
}

$exeSource = Join-Path $repoRoot "target\release\codex-voice.exe"
if (-not (Test-Path $exeSource)) {
    throw "Expected build output not found: $exeSource"
}

$generatedInstallers = @(Get-ChildItem -Path (Join-Path $repoRoot "target\release\bundle\nsis") -Filter "*-setup.exe" -File)
if ($generatedInstallers.Count -ne 1) {
    throw "Expected exactly one NSIS installer, found $($generatedInstallers.Count)"
}
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
Copy-Item -Path $generatedInstallers[0].FullName -Destination $installerPath -Force

# --- 3. Stage --------------------------------------------------------------
if (Test-Path $stageDir) {
    Remove-Item -Recurse -Force $stageDir
}
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

Copy-Item -Path $exeSource -Destination (Join-Path $stageDir "codex-voice.exe") -Force

# README.txt -- how to run and how to autostart.
$readme = @'
Codex Voice for Windows (x64)
=============================

Codex Voice is a hold-to-dictate desktop utility backed by your local Codex
auth. It runs in the background with a system-tray icon.

Running
-------
Open a terminal in this folder and run:

    codex-voice.exe run

A tray icon appears. While it is running:

  * Hold Control-M to dictate. Speech is transcribed and pasted into the
    focused application when you release the keys.
  * Press Win-F6 to speak the currently selected text aloud.

To confirm the executable is healthy without starting the tray:

    codex-voice.exe --version
    codex-voice.exe doctor codex-auth

Autostart at login
------------------
Run the bundled helper once to create a shortcut in your Startup folder so
Codex Voice launches automatically when you sign in:

    powershell -ExecutionPolicy Bypass -File install-autostart.ps1

To remove autostart, delete the shortcut:

    %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\Codex Voice.lnk

Notes
-----
If Windows reports that WebView2 is missing, use the sibling NSIS setup
executable instead of this portable ZIP; the installer provisions WebView2.

Pasting uses SendInput. Windows User Interface Privilege Isolation (UIPI) can
block synthetic input into elevated (Administrator) windows. If a paste does
not land, make sure the target application is running without elevation, or
run codex-voice.exe at the same integrity level as the target.
'@
Set-Content -Path (Join-Path $stageDir "README.txt") -Value $readme -Encoding UTF8

# install-autostart.ps1 -- creates a Startup-folder shortcut to codex-voice.exe run.
$autostart = @'
#Requires -Version 5.1
# Creates a Startup-folder shortcut that launches "codex-voice.exe run" at login.
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$exePath = Join-Path $scriptDir "codex-voice.exe"
if (-not (Test-Path $exePath)) {
    throw "codex-voice.exe not found next to this script: $exePath"
}

$startup = [Environment]::GetFolderPath("Startup")
$shortcutPath = Join-Path $startup "Codex Voice.lnk"

$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $exePath
$shortcut.Arguments = "run"
$shortcut.WorkingDirectory = $scriptDir
$shortcut.Description = "Codex Voice hold-to-dictate utility"
$shortcut.Save()

Write-Host "Autostart shortcut created: $shortcutPath"
'@
Set-Content -Path (Join-Path $stageDir "install-autostart.ps1") -Value $autostart -Encoding UTF8

# --- 4. Zip ----------------------------------------------------------------
if (Test-Path $zipPath) {
    Remove-Item -Force $zipPath
}
Compress-Archive -Path (Join-Path $stageDir "*") -DestinationPath $zipPath -Force
if (-not (Test-Path $zipPath)) {
    throw "Zip was not produced: $zipPath"
}

# --- 5. Report -------------------------------------------------------------
$zipHash = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash
$zipSize = (Get-Item $zipPath).Length
$installerHash = (Get-FileHash -Algorithm SHA256 -Path $installerPath).Hash
$installerSize = (Get-Item $installerPath).Length
$installerHashPath = "$installerPath.sha256"
$zipHashPath = "$zipPath.sha256"
Set-Content -Path $installerHashPath -Value "$installerHash  $([IO.Path]::GetFileName($installerPath))" -Encoding ASCII
Set-Content -Path $zipHashPath -Value "$zipHash  $([IO.Path]::GetFileName($zipPath))" -Encoding ASCII

Write-Host ""
Write-Host "Distribution built successfully."
Write-Host "  Installer:        $installerPath"
Write-Host "  Installer size:   $installerSize bytes"
Write-Host "  Installer SHA256: $installerHash"
Write-Host "  Installer hash:   $installerHashPath"
Write-Host "  Zip:              $zipPath"
Write-Host "  Zip size:         $zipSize bytes"
Write-Host "  Zip SHA256:       $zipHash"
Write-Host "  Zip hash:         $zipHashPath"
