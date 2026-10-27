# Builds the OmniZip MSIX package from release binaries (docs/10-build-release.md §3).
#
#   pwsh platform/windows/msix/pack.ps1 -Version 0.1.0 [-Publisher "CN=..."] [-Arch x64]
#                                       [-Sign path\to\cert.pfx -Password ...]
#
# Prerequisite: `tauri build` (or scripts/prepare-bundle.mjs plus a release build of
# omnizip-desktop) so that target/release and apps/desktop/src-tauri/binaries are filled.
# An unsigned package can be validated but not installed; CI signs it with the release
# certificate, whose subject must equal -Publisher.
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$Publisher = "CN=OmniZip Contributors",
  [ValidateSet("x64", "arm64")][string]$Arch = "x64",
  [string]$Out = "",
  [string]$Sign = "",
  [string]$Password = ""
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "../../..")
$triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }

# MSIX versions have four numeric parts; pre-release suffixes are dropped.
$numeric = ($Version -split "[-+]")[0]
$parts = @($numeric -split "\.")
while ($parts.Count -lt 4) { $parts += "0" }
$msixVersion = ($parts[0..3] -join ".")

function Find-SdkTool([string]$name) {
  $cmd = Get-Command $name -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
  $hit = Get-ChildItem $kits -Recurse -Filter $name -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "\\x64\\" } | Sort-Object FullName -Descending | Select-Object -First 1
  if (-not $hit) { throw "$name not found: install the Windows SDK" }
  return $hit.FullName
}

$stage = Join-Path $root "target/msix/$Arch"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage "Assets") | Out-Null

$release = Join-Path $root "target/release"
$bins = Join-Path $root "apps/desktop/src-tauri/binaries"
$files = @{
  (Join-Path $release "omnizip-desktop.exe")   = "omnizip-desktop.exe"
  (Join-Path $bins "omnizip-$triple.exe")      = "omnizip.exe"
  (Join-Path $bins "omnizip_shell.dll")        = "omnizip_shell.dll"
}
foreach ($src in $files.Keys) {
  if (-not (Test-Path $src)) { throw "missing $src (run the release build first)" }
  Copy-Item $src (Join-Path $stage $files[$src])
}
Copy-Item (Join-Path $root "LICENSE") (Join-Path $stage "LICENSE.txt")
$icons = Join-Path $root "apps/desktop/src-tauri/icons"
foreach ($logo in "StoreLogo", "Square44x44Logo", "Square150x150Logo") {
  Copy-Item (Join-Path $icons "$logo.png") (Join-Path $stage "Assets/$logo.png")
}

$manifest = Get-Content -Raw (Join-Path $PSScriptRoot "AppxManifest.xml")
$manifest = $manifest.Replace('$VERSION$', $msixVersion).Replace('$PUBLISHER$', $Publisher).Replace('$ARCH$', $Arch)
Set-Content -Path (Join-Path $stage "AppxManifest.xml") -Value $manifest -Encoding utf8

if (-not $Out) { $Out = Join-Path $root "target/release/bundle/msix/OmniZip_${Version}_$Arch.msix" }
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
& (Find-SdkTool "makeappx.exe") pack /o /d $stage /p $Out
if ($LASTEXITCODE -ne 0) { throw "makeappx failed ($LASTEXITCODE)" }

if ($Sign) {
  & (Find-SdkTool "signtool.exe") sign /fd SHA256 /f $Sign /p $Password /tr http://timestamp.digicert.com /td SHA256 $Out
  if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
}
Write-Output "MSIX: $Out"
