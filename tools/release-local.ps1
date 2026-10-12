<#
.SYNOPSIS
    Cuts a GitHub release built on this machine (no CI). See tools/release.ps1 for the CI-driven release.

.DESCRIPTION
    1. Checks that the working tree is clean, on main, and in sync with origin.
    2. Bumps [workspace.package] version in Cargo.toml (all crates inherit it)
       and commits "Release vX.Y.Z" when the version changes.
    3. Builds dumb-editor and dumb-player in release mode.
    4. Zips the binaries with README.md into target/dist/.
    5. Tags vX.Y.Z, pushes the commit and the tag, and creates the GitHub
       release with auto-generated notes and the zip attached.

    Requires git, cargo and an authenticated GitHub CLI (gh auth login).

.EXAMPLE
    ./tools/release-local.ps1 0.2.0
.EXAMPLE
    ./tools/release-local.ps1 0.3.0-beta.1 -Prerelease -Draft
.EXAMPLE
    ./tools/release-local.ps1 0.2.0 -DryRun
#>
[CmdletBinding()]
param(
    # Version to release, e.g. 0.2.0 or v0.2.0.
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Version,

    # Create the release as a draft.
    [switch]$Draft,

    # Mark the release as a pre-release.
    [switch]$Prerelease,

    # Branch releases must be cut from.
    [string]$Branch = "main",

    # Build and package only: no commit, tag, push or release.
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

function Invoke-Native {
    param([Parameter(Mandatory = $true)][string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "'$Exe $($Arguments -join ' ')' failed with exit code $LASTEXITCODE"
    }
}

function Step([string]$Message) {
    Write-Host "==> $Message" -ForegroundColor Cyan
}

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $root
try {
    # --- Validate input --------------------------------------------------
    $Version = $Version.TrimStart("v")
    if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') {
        throw "Version '$Version' is not semver (expected X.Y.Z or X.Y.Z-pre)."
    }
    $tag = "v$Version"

    foreach ($tool in "git", "cargo", "gh") {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
            throw "'$tool' was not found on PATH."
        }
    }

    # --- Repository checks -----------------------------------------------
    Step "Checking repository state"
    if (-not $DryRun) {
        & gh auth status *> $null
        if ($LASTEXITCODE -ne 0) { throw "GitHub CLI is not authenticated. Run 'gh auth login'." }
    }

    $current = (git rev-parse --abbrev-ref HEAD).Trim()
    if ($current -ne $Branch) { throw "On branch '$current'; releases are cut from '$Branch'." }

    if (git status --porcelain) { throw "Working tree is not clean. Commit or stash your changes first." }

    Invoke-Native git @("fetch", "--quiet", "--tags", "origin", $Branch)
    $local = (git rev-parse HEAD).Trim()
    $remote = (git rev-parse "origin/$Branch").Trim()
    if ($local -ne $remote) { throw "Local '$Branch' differs from 'origin/$Branch'. Pull or push first." }

    if (git tag --list $tag) { throw "Tag '$tag' already exists." }

    # --- Version bump ----------------------------------------------------
    $manifest = Join-Path $root "Cargo.toml"
    $text = [IO.File]::ReadAllText($manifest)
    $pattern = '(?m)(\[workspace\.package\]\s*\r?\nversion\s*=\s*")([^"]+)(")'
    $match = [regex]::Match($text, $pattern)
    if (-not $match.Success) { throw "Could not find [workspace.package] version in Cargo.toml." }
    $oldVersion = $match.Groups[2].Value

    $bumped = $oldVersion -ne $Version
    if ($bumped) {
        Step "Bumping version $oldVersion -> $Version"
        $text = [regex]::Replace($text, $pattern, "`${1}$Version`${3}", 1)
        [IO.File]::WriteAllText($manifest, $text, (New-Object System.Text.UTF8Encoding $false))
        Invoke-Native cargo @("update", "--workspace", "--quiet")
    } else {
        Step "Cargo.toml is already at $Version"
    }

    # --- Build -----------------------------------------------------------
    Step "Building release binaries"
    Invoke-Native cargo @("build", "--release", "--locked",
        "-p", "dumb_editor", "--bin", "dumb-editor",
        "-p", "dumb_runtime", "--bin", "dumb-player")

    # --- Package ---------------------------------------------------------
    Step "Packaging"
    $exe = if ($env:OS -eq "Windows_NT") { ".exe" } else { "" }
    $platform = if ($env:OS -eq "Windows_NT") { "windows-x64" } else { "$([Environment]::OSVersion.Platform)".ToLower() }
    $name = "dumb-engine-$tag-$platform"
    $dist = Join-Path $root "target/dist"
    $stage = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"

    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    if (Test-Path $zip) { Remove-Item $zip -Force }
    New-Item -ItemType Directory -Force $stage | Out-Null

    foreach ($bin in "dumb-editor", "dumb-player") {
        Copy-Item (Join-Path $root "target/release/$bin$exe") $stage
    }
    Copy-Item (Join-Path $root "README.md") $stage
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
    Write-Host "    $zip"

    if ($DryRun) {
        if ($bumped) {
            Invoke-Native git @("checkout", "--", "Cargo.toml", "Cargo.lock")
            Write-Host "    Reverted the version bump."
        }
        Step "Dry run finished: nothing was committed, tagged, pushed or released."
        return
    }

    # --- Commit, tag, push -----------------------------------------------
    if ($bumped) {
        Step "Committing version bump"
        Invoke-Native git @("add", "Cargo.toml", "Cargo.lock")
        Invoke-Native git @("commit", "--quiet", "-m", "Release $tag")
    }

    Step "Tagging $tag"
    Invoke-Native git @("tag", "-a", $tag, "-m", "Release $tag")

    Step "Pushing to origin"
    Invoke-Native git @("push", "--quiet", "origin", $Branch)
    Invoke-Native git @("push", "--quiet", "origin", $tag)

    # --- GitHub release --------------------------------------------------
    Step "Creating GitHub release"
    $ghArgs = @("release", "create", $tag, $zip, "--title", "Dumb Engine $tag", "--generate-notes", "--verify-tag")
    if ($Draft) { $ghArgs += "--draft" }
    if ($Prerelease) { $ghArgs += "--prerelease" }
    Invoke-Native gh $ghArgs

    Step "Released $tag"
}
finally {
    Pop-Location
}
