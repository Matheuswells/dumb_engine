#!/usr/bin/env bash
# Cut a release: bump the workspace version, commit, tag vX.Y.Z and push. The tag starts
# .github/workflows/release.yml, which builds the Windows and Linux bundles and publishes the
# GitHub release.
#
#   tools/release.sh 0.2.0              release 0.2.0
#   tools/release.sh 0.2.0 --dry-run    show what would happen, change nothing
#   tools/release.sh 0.2.0 --skip-tests skip the local cargo test run
#
# On Windows, tools/release.ps1 does the same from PowerShell.
set -euo pipefail

usage() { sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

version=""
dry_run=0
skip_tests=0
branch="main"
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=1 ;;
        --skip-tests) skip_tests=1 ;;
        --branch=*) branch="${arg#--branch=}" ;;
        -h|--help) usage ;;
        -*) echo "unknown option $arg" >&2; usage ;;
        *) version="${arg#v}" ;;
    esac
done
[[ -n "$version" ]] || usage
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "error: '$version' is not a version like 1.2.3" >&2; exit 1; }
tag="v$version"

cd "$(dirname "${BASH_SOURCE[0]}")/.."
step() { printf '\n==> %s\n' "$*"; }
run() {
    if (( dry_run )); then echo "(dry run) $*"; else "$@"; fi
}
die() { echo "error: $*" >&2; exit 1; }

current="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' Cargo.toml)"
[[ -n "$current" ]] || die "could not find [workspace.package] version in Cargo.toml"

step "Checking the repository"
[[ -z "$(git status --porcelain)" ]] || die "the working tree has uncommitted changes; commit or stash them first"
[[ "$(git rev-parse --abbrev-ref HEAD)" == "$branch" ]] || die "releases are cut from '$branch' (you are on '$(git rev-parse --abbrev-ref HEAD)'); use --branch=<name> to override"
git fetch --quiet origin "$branch" --tags
[[ "$(git rev-parse HEAD)" == "$(git rev-parse "origin/$branch")" ]] || die "'$branch' differs from origin/$branch; pull or push first"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists"
if [[ "$current" == "$version" ]]; then
    echo "Cargo.toml is already at $version"
elif [[ "$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n1)" != "$version" ]]; then
    die "$version is lower than the current version $current"
fi
echo "Releasing $current -> $version (tag $tag)"

step "Bumping the version"
if (( dry_run )); then
    echo "(dry run) set [workspace.package] version = \"$version\" in Cargo.toml"
else
    sed -i.bak "/^\[workspace.package\]/,/^\[/s/^version *= *\".*\"/version = \"$version\"/" Cargo.toml && rm -f Cargo.toml.bak
    cargo update --workspace --quiet # refresh the crate versions in Cargo.lock
fi

if (( skip_tests )); then
    step "Skipping tests (--skip-tests)"
else
    step "Running tests"
    run cargo test --workspace --quiet
fi

step "Committing and tagging"
if [[ "$current" != "$version" ]]; then
    run git commit -am "Release $tag"
fi
run git tag -a "$tag" -m "Dumb Engine $tag"

step "Pushing"
run git push origin "$branch"
run git push origin "$tag"

remote="$(git remote get-url origin | sed -E 's#(git@|https?://)([^/:]+)[:/]#https://\2/#; s#\.git$##')"
echo
if (( dry_run )); then
    echo "Dry run finished; nothing was changed."
else
    echo "Pushed $tag. GitHub Actions is building the release:"
    echo "  $remote/actions/workflows/release.yml"
    echo "It will appear at $remote/releases/tag/$tag"
fi
