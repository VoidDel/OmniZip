# Creates a self-signed code-signing certificate for TEST MSIX packages (never for releases).
#
#   pwsh platform/windows/msix/new-test-cert.ps1 [-Publisher "CN=OmniZip Contributors"] [-Trust]
#
# Writes omnizip-test.pfx, omnizip-test.cer and password.txt to -OutDir (default
# %LOCALAPPDATA%\OmniZip\dev-cert, outside the repository: never commit the .pfx).
# -Trust imports the public certificate into LocalMachine\TrustedPeople so that packages
# signed with it install on this machine; that needs administrator rights and shows a UAC
# prompt. Every other test machine needs the same import (the .cer file is enough).
#
# Then:  pwsh platform/windows/msix/pack.ps1 -Version 0.1.0 -Sign <OutDir>\omnizip-test.pfx `
#             -Password (Get-Content <OutDir>\password.txt)
#        Add-AppxPackage target\release\bundle\msix\OmniZip_0.1.0_x64.msix
#
# Remove later:
#        Get-AppxPackage OmniZip.OmniZip | Remove-AppxPackage
#        (admin) Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -eq "<Publisher>" | Remove-Item
#        Get-ChildItem Cert:\CurrentUser\My | Where-Object FriendlyName -eq "OmniZip test signing" | Remove-Item
param(
  # Must equal the Publisher the package is built with (pack.ps1 -Publisher).
  [string]$Publisher = "CN=OmniZip Contributors",
  [string]$OutDir = (Join-Path $env:LOCALAPPDATA "OmniZip\dev-cert"),
  [int]$Years = 2,
  [switch]$Trust
)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$pfx = Join-Path $OutDir "omnizip-test.pfx"
$cer = Join-Path $OutDir "omnizip-test.cer"
$pwFile = Join-Path $OutDir "password.txt"

if (Test-Path $pfx) {
  Write-Output "Reusing $pfx"
} else {
  $cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher `
    -FriendlyName "OmniZip test signing" `
    -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 3072 `
    -CertStoreLocation Cert:\CurrentUser\My `
    -NotAfter (Get-Date).AddYears($Years) `
    -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
  # A random password protects the exported key; it lives next to the .pfx because this
  # certificate is for local testing only.
  $password = -join ((48..57) + (65..90) + (97..122) | Get-Random -Count 24 | ForEach-Object { [char]$_ })
  Set-Content -Path $pwFile -Value $password -NoNewline
  Export-PfxCertificate -Cert $cert -FilePath $pfx -Password (ConvertTo-SecureString $password -AsPlainText -Force) | Out-Null
  Export-Certificate -Cert $cert -FilePath $cer | Out-Null
  Write-Output "Created $pfx (thumbprint $($cert.Thumbprint))"
}

if ($Trust) {
  $import = "Import-Certificate -FilePath '$cer' -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null"
  $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
  if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Invoke-Expression $import
  } else {
    $shell = (Get-Process -Id $PID).Path
    $p = Start-Process -FilePath $shell -Verb RunAs -Wait -PassThru -ArgumentList @("-NoProfile", "-Command", $import)
    if ($p.ExitCode -ne 0) { throw "Importing the certificate failed (exit $($p.ExitCode))" }
  }
  $thumb = (New-Object Security.Cryptography.X509Certificates.X509Certificate2 $cer).Thumbprint
  if (-not (Test-Path "Cert:\LocalMachine\TrustedPeople\$thumb")) { throw "Certificate is not in LocalMachine\TrustedPeople" }
  Write-Output "Trusted on this machine: LocalMachine\TrustedPeople\$thumb"
}
Write-Output "Certificate: $cer"
