<#
.SYNOPSIS
    Builds ro_launcher in release mode and publishes it to the project root.

.DESCRIPTION
    This script lives inside the crate (which is also the git repository). The
    shipped layout it produces is:

        RO_PreRenewal\
            ro_launcher.exe        <- built here
            ro_launcher.toml       <- configuration (refreshed from the repo)
            README.md              <- copied from the repo for convenience
            ro_launcher\           <- the crate / repository
                Cargo.toml, src\, build.ps1, ...
            1_Database\  2_Server\  3_Client\

    The root copy of ro_launcher.toml is ALWAYS refreshed from the template in
    this folder, so the repository stays the single source of truth.

    Development builds stay in ro_launcher\target\debug and are located
    automatically, so nothing else is needed while working on the launcher.

.EXAMPLE
    .\build.ps1
    .\build.ps1 -Configuration debug
#>
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release'
)

$ErrorActionPreference = 'Stop'

$crate = $PSScriptRoot
$root = Split-Path -Parent $crate

Push-Location $crate
try {
    Write-Host "==> cargo build --$Configuration" -ForegroundColor Cyan
    & cargo build "--$Configuration"
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

$built = Join-Path $crate "target\$Configuration\ro_launcher.exe"
if (-not (Test-Path -LiteralPath $built)) {
    throw "Expected binary was not produced: $built"
}

Write-Host "==> publishing to the project root" -ForegroundColor Cyan
$exe = Join-Path $root 'ro_launcher.exe'
Copy-Item -LiteralPath $built -Destination $exe -Force

# The repository is the single source of truth for the configuration.
foreach ($file in @('ro_launcher.toml', 'README.md')) {
    $src = Join-Path $crate $file
    if (Test-Path -LiteralPath $src) {
        Copy-Item -LiteralPath $src -Destination (Join-Path $root $file) -Force
    }
}

$info = Get-Item -LiteralPath $exe
$hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.Substring(0, 16)
Write-Host ""
Write-Host "    $($info.FullName)" -ForegroundColor Green
Write-Host ("    {0:N0} bytes   sha256:{1}..." -f $info.Length, $hash) -ForegroundColor DarkGray
Write-Host ""
Write-Host "Done. Launch the stack from the project root:" -ForegroundColor Cyan
Write-Host "    .\$($info.Name)" -ForegroundColor White