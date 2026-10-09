<#
.SYNOPSIS
    Cut a release: bump the workspace version, commit, tag vX.Y.Z and push.

.DESCRIPTION
    The pushed tag starts .github/workflows/release.yml, which builds the Windows and Linux
    bundles and publishes the GitHub release. Same steps as tools/release.sh.

.EXAMPLE
    .\tools\release.ps1 0.2.0
.EXAMPLE
    .\tools\release.ps1 0.2.0 -DryRun
.EXAMPLE
    .\tools\release.ps1 0.2.0 -SkipTests
#>
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Version,
    [switch]$DryRun,
    [switch]$SkipTests,
    [string]$Branch = "main"
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Die($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }
# Run a native command (dry run: only print it) and stop on failure.
function Run([string]$exe, [string[]]$cmdArgs) {
    if ($DryRun) { Write-Host "(dry run) $exe $($cmdArgs -join ' ')"; return }
    & $exe @cmdArgs
    if ($LASTEXITCODE -ne 0) { Die "$exe $($cmdArgs -join ' ') failed" }
}
function Get-GitOutput([string[]]$cmdArgs) {
    $out = & git @cmdArgs
    if ($LASTEXITCODE -ne 0) { Die "git $($cmdArgs -join ' ') failed" }
    return $out
}

$Version = $Version.TrimStart("v")
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') { Die "'$Version' is not a version like 1.2.3" }
$tag = "v$Version"

# [workspace.package] version in Cargo.toml.
$toml = Get-Content Cargo.toml -Raw
$section = [regex]::Match($toml, '(?ms)^\[workspace\.package\]\s*$(.*?)(?=^\[|\z)')
if (-not $section.Success) { Die "no [workspace.package] in Cargo.toml" }
$current = [regex]::Match($section.Groups[1].Value, '(?m)^version\s*=\s*"([^"]*)"').Groups[1].Value
if (-not $current) { Die "could not find [workspace.package] version in Cargo.toml" }

Step "Checking the repository"
if (Get-GitOutput @("status", "--porcelain")) { Die "the working tree has uncommitted changes; commit or stash them first" }
$head = Get-GitOutput @("rev-parse", "--abbrev-ref", "HEAD")
if ($head -ne $Branch) { Die "releases are cut from '$Branch' (you are on '$head'); use -Branch <name> to override" }
Get-GitOutput @("fetch", "--quiet", "origin", $Branch, "--tags") | Out-Null
if ((Get-GitOutput @("rev-parse", "HEAD")) -ne (Get-GitOutput @("rev-parse", "origin/$Branch"))) { Die "'$Branch' differs from origin/$Branch; pull or push first" }
& git rev-parse -q --verify "refs/tags/$tag" *> $null
if ($LASTEXITCODE -eq 0) { Die "tag $tag already exists" }
$cmpCurrent = [version]($current -replace '-.*$', '')
$cmpNew = [version]($Version -replace '-.*$', '')
if ($cmpNew -lt $cmpCurrent) { Die "$Version is lower than the current version $current" }
Write-Host "Releasing $current -> $Version (tag $tag)"

Step "Bumping the version"
if ($DryRun) {
    Write-Host "(dry run) set [workspace.package] version = `"$Version`" in Cargo.toml"
} elseif ($current -ne $Version) {
    $body = [regex]::Replace($section.Groups[1].Value, '(?m)^version\s*=\s*"[^"]*"', "version = `"$Version`"", 1)
    $toml = $toml.Substring(0, $section.Groups[1].Index) + $body + $toml.Substring($section.Groups[1].Index + $section.Groups[1].Length)
    [System.IO.File]::WriteAllText((Resolve-Path Cargo.toml), $toml)
    Run cargo @("update", "--workspace", "--quiet") # refresh the crate versions in Cargo.lock
}

if ($SkipTests) {
    Step "Skipping tests (-SkipTests)"
} else {
    Step "Running tests"
    Run cargo @("test", "--workspace", "--quiet")
}

Step "Committing and tagging"
if ($current -ne $Version) { Run git @("commit", "-am", "Release $tag") }
Run git @("tag", "-a", $tag, "-m", "Dumb Engine $tag")

Step "Pushing"
Run git @("push", "origin", $Branch)
Run git @("push", "origin", $tag)

$remote = (Get-GitOutput @("remote", "get-url", "origin")) -replace '^(git@|https?://)([^/:]+)[:/]', 'https://$2/' -replace '\.git$', ''
Write-Host ""
if ($DryRun) {
    Write-Host "Dry run finished; nothing was changed."
} else {
    Write-Host "Pushed $tag. GitHub Actions is building the release:" -ForegroundColor Green
    Write-Host "  $remote/actions/workflows/release.yml"
    Write-Host "It will appear at $remote/releases/tag/$tag"
}
