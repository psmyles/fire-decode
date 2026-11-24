<#
.SYNOPSIS
Tag and publish a fire-decode release from PowerShell. A wrapper around scripts/release.sh.

.DESCRIPTION
Finds Git for Windows' bash and runs scripts/release.sh in it, with three conveniences:

- Without -Assets, it downloads the HEIF source tarballs (libheif, libde265, dav1d) from the
  latest GitHub release into a temporary folder, and deletes that folder afterwards. That is right
  while the vendored HEIF stack is unchanged. After a re-vendor, pass -Assets with vcpkg's
  downloads folder. release.sh checks each tarball's version against VENDOR.txt either way, so
  stale tarballs stop the release instead of shipping.
- It always runs release.sh --dry-run first, so the checks, the files and the notes are on screen.
- Without -DryRun, it then asks before tagging. -Yes skips the question.

The version comes from product.json, as with release.sh. Bump it, commit, push main, and wait for
CI to pass first (README, "Versions and releases").

.PARAMETER Assets
A folder holding the upstream source tarballs. Omit it to take them from the latest release.

.PARAMETER DryRun
Run the checks and print the release notes, then stop: no tag, no release.

.PARAMETER Yes
Don't ask before tagging.

.EXAMPLE
scripts\release.ps1 -DryRun

.EXAMPLE
scripts\release.ps1

.EXAMPLE
scripts\release.ps1 -Assets D:\Dev\vcpkg\downloads
#>
[CmdletBinding()]
param(
    [string]$Assets,
    [switch]$DryRun,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

# Git for Windows' bash. Not `Get-Command bash`: on Windows that can be WSL's
# C:\Windows\System32\bash.exe, which can't see this checkout's git or gh.
function Find-GitBash {
    $candidates = @()
    if ($env:ProgramFiles) { $candidates += Join-Path $env:ProgramFiles 'Git\bin\bash.exe' }
    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if ($git) {
        # git.exe is in <Git>\cmd, <Git>\bin or <Git>\mingw64\bin.
        $dir = Split-Path -Parent $git.Source
        $candidates += Join-Path $dir 'bash.exe'
        $candidates += Join-Path $dir '..\bin\bash.exe'
        $candidates += Join-Path $dir '..\..\bin\bash.exe'
    }
    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) { return (Resolve-Path -LiteralPath $c).Path }
    }
    throw "Git for Windows' bash.exe wasn't found. Install Git for Windows (https://gitforwindows.org)."
}

function Invoke-Release([string]$bash, [string]$assetsDir, [bool]$dry) {
    $bashArgs = @('scripts/release.sh', ($assetsDir -replace '\\', '/'))
    if ($dry) { $bashArgs += '--dry-run' }
    # Out-Host, so the script's output reaches the screen instead of this function's return value.
    & $bash @bashArgs | Out-Host
    return $LASTEXITCODE
}

$bash = Find-GitBash
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw 'The GitHub CLI (gh) is needed, signed in: https://cli.github.com'
}
$version = (Get-Content -Raw -LiteralPath (Join-Path $repo 'product.json') | ConvertFrom-Json).version

$temp = $null
Push-Location $repo
try {
    if ($Assets) {
        if (-not (Test-Path -LiteralPath $Assets -PathType Container)) {
            throw "-Assets: no folder at $Assets"
        }
        $assetsDir = (Resolve-Path -LiteralPath $Assets).Path
    } else {
        $previous = gh release view --json tagName --jq .tagName
        if ($LASTEXITCODE -ne 0 -or -not $previous) { throw 'No previous GitHub release to take the tarballs from; pass -Assets.' }
        $temp = Join-Path ([IO.Path]::GetTempPath()) ('fire-decode-release-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $temp | Out-Null
        Write-Host "== source tarballs from $previous"
        gh release download $previous --pattern '*.tar.gz' --dir $temp
        if ($LASTEXITCODE -ne 0) { throw "Downloading the tarballs from $previous failed." }
        $assetsDir = $temp
    }

    $code = Invoke-Release $bash $assetsDir $true
    if ($code -ne 0 -or $DryRun) { exit $code }

    if (-not $Yes) {
        $answer = Read-Host "Tag v$version, push the tag and publish the GitHub release? [y/N]"
        if ($answer -notmatch '^(y|yes)$') {
            Write-Host '== stopped: no tag, no release'
            exit 1
        }
    }
    exit (Invoke-Release $bash $assetsDir $false)
} finally {
    Pop-Location
    if ($temp) { Remove-Item -Recurse -Force -LiteralPath $temp -ErrorAction SilentlyContinue }
}
