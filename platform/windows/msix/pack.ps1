# Builds the OmniZip MSIX package from release binaries (docs/10-build-release.md §3).
#
#   pwsh platform/windows/msix/pack.ps1 -Version 0.1.0 [-Publisher "CN=..."] [-Arch x64]
#                                       [-Sign path\to\cert.pfx -Password ...] [-Sparse]
#
# -Sparse builds the context-menu package of the EXE install instead (AppxManifest.sparse.xml):
# a sparse package with the logos only, registered by the installer with the install folder as
# its external location (scripts/prepare-bundle.mjs builds it into the NSIS installer).
#
# Prerequisite: `pnpm bundle:windows` (or scripts/prepare-bundle.mjs plus a release build of
# omnizip-desktop) so that target/release and apps/desktop/src-tauri/binaries are filled.
# An unsigned package can be validated but not installed; CI signs it with the release
# certificate, whose subject must equal -Publisher.
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$Publisher = "CN=OmniZip Contributors",
  [ValidateSet("x64", "arm64")][string]$Arch = "x64",
  [string]$Out = "",
  [string]$Sign = "",
  [string]$Password = "",
  [switch]$Sparse
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

$stage = Join-Path $root ("target/msix/" + $(if ($Sparse) { "sparse-$Arch" } else { $Arch }))
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage "Assets") | Out-Null

$release = Join-Path $root "target/release"
$bins = Join-Path $root "apps/desktop/src-tauri/binaries"
$files = @{
  (Join-Path $release "omnizip-desktop.exe")   = "omnizip-desktop.exe"
  (Join-Path $bins "omnizip-$triple.exe")      = "omnizip.exe"
  (Join-Path $bins "omnizip_shell.dll")        = "omnizip_shell.dll"
}
# The sparse package holds no files of its own: they are those of the install folder.
if (-not $Sparse) {
  foreach ($src in $files.Keys) {
    if (-not (Test-Path $src)) { throw "missing $src (run the release build first)" }
    Copy-Item $src (Join-Path $stage $files[$src])
  }
  Copy-Item (Join-Path $root "LICENSE") (Join-Path $stage "LICENSE.txt")
  # Icons of the context-menu items, next to omnizip_shell.dll.
  Copy-Item -Recurse (Join-Path $root "icons/menu") (Join-Path $stage "menu-icons")
}

# Logos. The shell picks the taskbar, Start and file-type icons by pixel size through
# resources.pri; given only the plain 44x44 logo it scales that onto a plate, which looks small
# and blurred next to the exe icon. The variants are rendered from the artwork of the exe icon.
Add-Type -AssemblyName System.Drawing
$art = [System.Drawing.Image]::FromFile((Join-Path $root "apps/desktop/src-tauri/icons/icon.png"))
function Save-Logo([string]$name, [int]$size, [double]$fill = 1.0) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
  $side = [int][math]::Round($size * $fill)
  $off = [int][math]::Floor(($size - $side) / 2)
  $wrap = New-Object System.Drawing.Imaging.ImageAttributes
  $wrap.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
  $g.DrawImage($art, (New-Object System.Drawing.Rectangle $off, $off, $side, $side), 0, 0, $art.Width, $art.Height, [System.Drawing.GraphicsUnit]::Pixel, $wrap)
  $g.Dispose()
  $bmp.Save((Join-Path $stage "Assets/$name"), [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
}
# The package logo (StoreLogo) is shown by App Installer on a square plate of the accent colour,
# which would show around the icon's rounded corners: it is the square inside the artwork's
# rounded one instead (icons/app-icon.svg: a 896-unit square from 64, corner radius 208, so the
# corners cut ~61 units in along the diagonal; 68 leaves a margin), scaled up to fill the tile.
function Save-Tile([string]$name, [int]$size) {
  $bmp = New-Object System.Drawing.Bitmap $size, $size
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
  $side = $size * 1024.0 / 760.0
  $off = ($size - $side) / 2
  $g.DrawImage($art, (New-Object System.Drawing.RectangleF $off, $off, $side, $side))
  $g.Dispose()
  $bmp.Save((Join-Path $stage "Assets/$name"), [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
}
foreach ($t in 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256) {
  Save-Logo "Square44x44Logo.targetsize-$t.png" $t
  Save-Logo "Square44x44Logo.targetsize-${t}_altform-unplated.png" $t
}
foreach ($s in 100, 125, 150, 200, 400) {
  $k = $s / 100
  Save-Logo "Square44x44Logo.scale-$s.png" ([math]::Ceiling(44 * $k))
  Save-Logo "Square150x150Logo.scale-$s.png" ([math]::Ceiling(150 * $k)) 0.66
  Save-Tile "StoreLogo.scale-$s.png" ([math]::Ceiling(50 * $k))
}
$art.Dispose()

$manifest = Get-Content -Raw (Join-Path $PSScriptRoot $(if ($Sparse) { "AppxManifest.sparse.xml" } else { "AppxManifest.xml" }))
$manifest = $manifest.Replace('$VERSION$', $msixVersion).Replace('$PUBLISHER$', $Publisher).Replace('$ARCH$', $Arch)
Set-Content -Path (Join-Path $stage "AppxManifest.xml") -Value $manifest -Encoding utf8

# resources.pri maps each logo of the manifest to its scale and target-size variants. The
# default config splits scales into separate resource packs, which only a bundle installs, so
# everything is kept in the one resources.pri.
$makepri = Find-SdkTool "makepri.exe"
$priConfig = Join-Path $root ("target/msix/priconfig-" + $(if ($Sparse) { "sparse-$Arch" } else { $Arch }) + ".xml")
& $makepri createconfig /cf $priConfig /dq en-US /pv 10.0.0 /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri createconfig failed ($LASTEXITCODE)" }
$cfg = (Get-Content -Raw $priConfig) -replace '(?s)\s*<packaging>.*?</packaging>', ''
Set-Content -Path $priConfig -Value $cfg -Encoding utf8
& $makepri new /pr $stage /cf $priConfig /mn (Join-Path $stage "AppxManifest.xml") /of (Join-Path $stage "resources.pri") /o | Out-Null
if ($LASTEXITCODE -ne 0) { throw "makepri new failed ($LASTEXITCODE)" }

if (-not $Out) {
  $name = if ($Sparse) { "OmniZip-ContextMenu_${Version}_$Arch.msix" } else { "OmniZip_${Version}_$Arch.msix" }
  $Out = Join-Path $root "target/release/bundle/msix/$name"
}
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
$makeappx = Find-SdkTool "makeappx.exe"
if ($Sparse) {
  # The files a sparse package names are in its external location: not checked here.
  & $makeappx pack /o /nv /d $stage /p $Out
} else {
  & $makeappx pack /o /d $stage /p $Out
}
if ($LASTEXITCODE -ne 0) { throw "makeappx failed ($LASTEXITCODE)" }

if ($Sign) {
  & (Find-SdkTool "signtool.exe") sign /fd SHA256 /f $Sign /p $Password /tr http://timestamp.digicert.com /td SHA256 $Out
  if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
}
Write-Output "MSIX: $Out"
