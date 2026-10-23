$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$installer = Join-Path $PSScriptRoot '../../install.ps1'
$fixtureRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("harn-installer-proof-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
$env:HARN_INSTALL_DIR = Join-Path $fixtureRoot 'installed'
$env:HARN_NO_MODIFY_PATH = '1'
$env:HARN_NO_VERIFY = '0'
try {
    & $installer
    $installed = $env:HARN_INSTALL_DIR
    Remove-Item Env:HARN_INSTALL_DIR, Env:HARN_NO_MODIFY_PATH, Env:HARN_NO_VERIFY
    $versionJson = & (Join-Path $installed 'harn.exe') version --json
    if ($LASTEXITCODE -ne 0 -or -not ($versionJson | ConvertFrom-Json).ok) {
        throw 'The installed CLI did not execute its version command'
    }
    foreach ($name in @('LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')) {
        $file = Get-Item -LiteralPath (Join-Path $installed "harn-licenses/$name")
        if ($file.Length -eq 0) { throw "Installed license is empty: $name" }
    }
    $before = (Get-FileHash -LiteralPath (Join-Path $installed 'harn.exe')).Hash

    $incomplete = Join-Path $fixtureRoot 'incomplete.zip'
    Compress-Archive -LiteralPath (Join-Path $installed 'harn.exe') -DestinationPath $incomplete
    $checksum = (Get-FileHash -LiteralPath $incomplete -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifest = Join-Path $fixtureRoot 'SHA256SUMS'
    "$checksum  harn-x86_64-pc-windows-msvc.zip" | Set-Content -LiteralPath $manifest
    function Invoke-WebRequest {
        param([string]$Uri, [string]$OutFile, [hashtable]$Headers)
        $source = if ($Uri.EndsWith('/SHA256SUMS')) { $manifest } else { $incomplete }
        Copy-Item -LiteralPath $source -Destination $OutFile
    }
    $env:HARN_VERSION = 'v0.10.999'
    $env:HARN_INSTALL_DIR = $installed
    $env:HARN_NO_MODIFY_PATH = '1'
    $env:HARN_NO_VERIFY = '0'
    $refused = $false
    try { & $installer } catch {
        if (-not $_.Exception.Message.Contains('LICENSE-MIT')) { throw }
        $refused = $true
    }
    if (-not $refused) { throw 'The installer accepted a checksummed archive without licenses' }
    if ((Get-FileHash -LiteralPath (Join-Path $installed 'harn.exe')).Hash -ne $before) {
        throw 'The rejected archive changed the installed CLI'
    }
    Write-Host 'Published Windows CLI executed; licenses retained; incomplete archive refused without replacing the CLI.'
}
finally {
    Remove-Item Env:HARN_VERSION, Env:HARN_INSTALL_DIR, Env:HARN_NO_MODIFY_PATH, Env:HARN_NO_VERIFY -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
}
