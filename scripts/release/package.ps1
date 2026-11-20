# Packages a release build of clipuh for Windows x64.
#
#   pwsh scripts/release/package.ps1 -Version 0.1.0
#
# Expects `cargo build --release --locked -p clipper-app --bin clipuh` to have
# run. Writes to -OutDir (default: dist):
#   clipuh-v<Version>-windows-x64.zip         clipuh.exe and third-party notices
#   clipuh-v<Version>-windows-x64.zip.sha256  "<hash>  <zip name>" (sha256sum -c)
#
# clipuh.exe is self-contained: icons, sounds and the app icon are compiled in,
# and Media Foundation, D3D11 and WASAPI ship with Windows. If the app ever
# needs files next to the executable, add them to $payload below.
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Exe = 'target/release/clipuh.exe',
    [string]$OutDir = 'dist'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    throw "Release executable not found: $Exe (run cargo build --release --locked -p clipper-app --bin clipuh)"
}

# Cheap sanity check of the PE header: a 64-bit x64 image with the GUI
# subsystem (release builds must not open a console window).
$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Exe))
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550) { throw "$Exe is not a PE image" }
$machine = [BitConverter]::ToUInt16($bytes, $pe + 4)
if ($machine -ne 0x8664) { throw ('{0} targets machine 0x{1:X4}, expected x64 (0x8664)' -f $Exe, $machine) }
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 24 + 68)
if ($subsystem -ne 2) { throw "$Exe uses subsystem $subsystem, expected 2 (Windows GUI)" }

$name = "clipuh-v$Version-windows-x64"
$stage = Join-Path $OutDir $name
$zip = Join-Path $OutDir "$name.zip"
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null

# Everything the portable build needs at run time.
$payload = @(
    @{ From = $Exe; To = 'clipuh.exe' }
    @{ From = Join-Path $PSScriptRoot '../../THIRD_PARTY_NOTICES.md'; To = 'THIRD_PARTY_NOTICES.md' }
)
foreach ($item in $payload) {
    Copy-Item -LiteralPath $item.From -Destination (Join-Path $stage $item.To)
}

Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal
$hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
# LF line ending and no BOM, so `sha256sum -c` reads it on any platform.
[System.IO.File]::WriteAllText("$zip.sha256", "$hash  $name.zip`n")

# The archive must hold exactly the payload.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $zip))
try {
    $entries = @($archive.Entries | ForEach-Object FullName | Sort-Object)
} finally {
    $archive.Dispose()
}
$expected = @($payload | ForEach-Object { $_.To } | Sort-Object)
if (Compare-Object $entries $expected) {
    throw "Unexpected archive contents: $($entries -join ', ')"
}

Remove-Item -LiteralPath $stage -Recurse -Force
Write-Host "Packaged $zip ($((Get-Item -LiteralPath $zip).Length) bytes)"
Write-Host "SHA-256 $hash"
