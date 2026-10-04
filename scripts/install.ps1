<#
.SYNOPSIS
  Specio installer for Windows (x86_64).

.EXAMPLE
  irm https://github.com/dayatech-labs/specio-cli/releases/latest/download/install.ps1 | iex

.EXAMPLE
  & ([scriptblock]::Create((irm https://github.com/dayatech-labs/specio-cli/releases/latest/download/install.ps1))) -Version 1.4.0

.DESCRIPTION
  Installs specio.exe into %LOCALAPPDATA%\Specio\bin without administrator rights. The download is
  verified against SHA256SUMS (fetched over HTTPS) before it is installed. The installer never asks
  for credentials or workspace URLs; run `specio login` afterwards.
#>
[CmdletBinding()]
param([string]$Version = "")

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Releases = if ($env:SPECIO_RELEASE_URL) { $env:SPECIO_RELEASE_URL } else { "https://github.com/dayatech-labs/specio-cli/releases" }
if ($Releases -notmatch '^(https://|http://127\.0\.0\.1|http://localhost)') { throw "the release URL must use https" }

$Version = $Version.TrimStart("v")
if ($Version -and $Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([0-9A-Za-z.+-]*)$') { throw "invalid version: $Version" }

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
if ($arch -ne [System.Runtime.InteropServices.Architecture]::X64) {
  throw "unsupported architecture: $arch. Download a build for your system from $Releases and install it manually."
}
$target = "x86_64-pc-windows-msvc"

$base = if ($Version) { "$Releases/download/v$Version" } else { "$Releases/latest/download" }
$binDir = Join-Path $env:LOCALAPPDATA "Specio\bin"
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("specio-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null
$staged = Join-Path $binDir (".specio-install-" + [guid]::NewGuid().ToString("N") + ".exe")

try {
  Write-Host "Fetching the release manifest..."
  $sumsPath = Join-Path $tmp "SHA256SUMS"
  Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sumsPath

  $line = Get-Content $sumsPath | Where-Object { $_ -match "^[0-9a-f]{64}\s+specio-[0-9][^\s]*-$target\.exe$" } | Select-Object -First 1
  if (-not $line) { throw "this release has no build for $target" }
  $expected, $name = $line -split '\s+', 2
  $found = $name -replace "^specio-", "" -replace "-$target\.exe$", ""
  if ($Version -and $found -ne $Version) { throw "release mismatch: wanted $Version, found $found" }

  Write-Host "Downloading specio $found for $target..."
  $download = Join-Path $tmp $name
  Invoke-WebRequest -UseBasicParsing -Uri "$Releases/download/v$found/$name" -OutFile $download

  $actual = (Get-FileHash -Algorithm SHA256 -Path $download).Hash.ToLowerInvariant()
  if ($actual -ne $expected) { throw "checksum mismatch; nothing was installed" }

  Copy-Item $download $staged
  & $staged --version *> $null
  if ($LASTEXITCODE -ne 0) { throw "the downloaded binary does not run on this system; nothing was installed" }

  $dest = Join-Path $binDir "specio.exe"
  if (Test-Path $dest) {
    # A running specio.exe cannot be overwritten, but it can be renamed away.
    $old = "$dest.old"
    Remove-Item -Force -ErrorAction SilentlyContinue $old
    Move-Item -Force $dest $old
    try { Move-Item -Force $staged $dest } catch { Move-Item -Force $old $dest; throw }
    Remove-Item -Force -ErrorAction SilentlyContinue $old
  } else {
    Move-Item -Force $staged $dest
  }
  Write-Host "Installed specio $found to $dest"

  # User PATH: add once, never duplicate.
  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  $entries = if ($userPath) { $userPath -split ';' | Where-Object { $_ } } else { @() }
  if ($entries -notcontains $binDir) {
    [Environment]::SetEnvironmentVariable("Path", (($entries + $binDir) -join ';'), "User")
    Write-Host ""
    Write-Host "$binDir was added to your user PATH. Open a new terminal, then run: specio login"
  } else {
    Write-Host "Next: run ``specio login``."
  }
} finally {
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
  if (Test-Path $staged) { Remove-Item -Force -ErrorAction SilentlyContinue $staged }
}
