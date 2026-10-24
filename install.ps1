<#
.SYNOPSIS
    Harn installer for Windows (PowerShell).

.DESCRIPTION
    Downloads the signed Windows release archive matching this machine,
    verifies it against the release's SHA256SUMS manifest, extracts the
    `harn`, `harn-dap`, and `harn-lsp` executables, retains required license
    material in `harn-licenses/`, and adds the install directory to PATH.

    Usage:
        irm https://harnlang.com/install.ps1 | iex

.NOTES
    Environment variables:
        HARN_VERSION         Pin a specific release tag, e.g. v0.8.151.
                             Defaults to the latest GitHub release.
        HARN_INSTALL_DIR     Install destination. Defaults to
                             "$env:LOCALAPPDATA\Programs\harn".
        HARN_NO_VERIFY       Set to 1 to skip SHA256 verification
                             (NOT recommended).
        HARN_NO_MODIFY_PATH  Set to 1 to leave PATH untouched.
#>

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = 'burin-labs/harn'
$ReleasesUrl = "https://github.com/$Repo/releases"
$ApiUrl = "https://api.github.com/repos/$Repo/releases"

function Write-Info { param([string]$Message) Write-Host "info: $Message" -ForegroundColor Cyan }
function Write-Done { param([string]$Message) Write-Host "ok: $Message" -ForegroundColor Green }
function Write-Warn { param([string]$Message) Write-Host "warning: $Message" -ForegroundColor Yellow }

# Detect architecture. Only x86_64 Windows binaries are published today; the
# msvc target runs under x64 emulation on arm64, so accept both.
$arch = $env:PROCESSOR_ARCHITECTURE
switch -Wildcard ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'x86_64-pc-windows-msvc' }
    default { throw "unsupported Windows architecture: $arch" }
}
$asset = "harn-$target.zip"

# Resolve the release tag.
$version = $env:HARN_VERSION
if ([string]::IsNullOrWhiteSpace($version)) {
    Write-Info 'Resolving latest release...'
    $latest = Invoke-RestMethod -Uri "$ApiUrl/latest" -Headers @{ 'User-Agent' = 'harn-install' }
    $version = $latest.tag_name
}
if ([string]::IsNullOrWhiteSpace($version)) {
    throw 'could not determine the release tag to install'
}
if ($version -notmatch '^v') {
    $version = "v$version"
}

$downloadBase = "$ReleasesUrl/download/$version"
$workDir = Join-Path ([System.IO.Path]::GetTempPath()) ("harn-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $workDir -Force | Out-Null

try {
    $archivePath = Join-Path $workDir $asset
    Write-Info "Downloading $asset ($version)..."
    Invoke-WebRequest -Uri "$downloadBase/$asset" -OutFile $archivePath -Headers @{ 'User-Agent' = 'harn-install' }

    if ($env:HARN_NO_VERIFY -eq '1') {
        Write-Warn 'HARN_NO_VERIFY=1 set — skipping checksum verification.'
    }
    else {
        Write-Info 'Verifying SHA256 checksum...'
        $sumsPath = Join-Path $workDir 'SHA256SUMS'
        Invoke-WebRequest -Uri "$downloadBase/SHA256SUMS" -OutFile $sumsPath -Headers @{ 'User-Agent' = 'harn-install' }

        $expectedLine = Get-Content $sumsPath | Where-Object { $_ -match [regex]::Escape($asset) } | Select-Object -First 1
        if (-not $expectedLine) {
            throw "no checksum entry for $asset in SHA256SUMS"
        }
        $expected = ($expectedLine -split '\s+')[0].ToLowerInvariant()
        $actual = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            throw "checksum mismatch for $asset (expected $expected, got $actual)"
        }
        Write-Done 'Checksum verified.'
    }

    $payload = Join-Path $workDir 'payload'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $payload
    $legalFiles = @('LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.txt')
    foreach ($name in @('harn.exe') + $legalFiles) {
        $file = Get-Item -LiteralPath (Join-Path $payload $name) -Force -ErrorAction SilentlyContinue
        if (-not $file -or $file.PSIsContainer -or $file.Length -eq 0 -or
            ($file.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
            throw "$asset has missing or invalid $name; expected a nonempty regular file"
        }
    }

    $installDir = $env:HARN_INSTALL_DIR
    if ([string]::IsNullOrWhiteSpace($installDir)) {
        $installDir = Join-Path $env:LOCALAPPDATA 'Programs\harn'
    }
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null

    $licenseDir = Join-Path $installDir 'harn-licenses'
    $executables = @('harn.exe', 'harn-dap.exe', 'harn-lsp.exe')
    $destinations = @($licenseDir) + @($legalFiles | ForEach-Object { Join-Path $licenseDir $_ }) +
        @($executables | ForEach-Object { Join-Path $installDir $_ })
    foreach ($destination in $destinations) {
        $item = Get-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
        if ($item -and (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -or
            ($destination -ne $licenseDir -and $item.PSIsContainer) -or
            ($destination -eq $licenseDir -and -not $item.PSIsContainer))) {
            throw "invalid install destination: $destination"
        }
    }
    New-Item -ItemType Directory -Path $licenseDir -Force | Out-Null
    foreach ($name in $legalFiles) {
        Copy-Item -LiteralPath (Join-Path $payload $name) -Destination $licenseDir -Force
    }

    Write-Info "Installing to $installDir..."

    foreach ($exe in $executables) {
        $source = Join-Path $payload $exe
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination (Join-Path $installDir $exe) -Force
        }
        else {
            Write-Warn "expected $exe was not found in the archive"
        }
    }

    if ($env:HARN_NO_MODIFY_PATH -eq '1') {
        Write-Warn "HARN_NO_MODIFY_PATH=1 set — add $installDir to PATH manually."
    }
    else {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @()
        if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ -ne '' } }
        if ($entries -notcontains $installDir) {
            $newPath = (@($entries) + $installDir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            $env:Path = "$env:Path;$installDir"
            Write-Done "Added $installDir to your user PATH (restart your shell to pick it up)."
        }
    }

    Write-Done "Installed harn $version. Run 'harn --help' to get started."
}
finally {
    Remove-Item -Path $workDir -Recurse -Force -ErrorAction SilentlyContinue
}
