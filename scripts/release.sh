#!/usr/bin/env bash
# Tag and publish a fire-decode release, named after product.json's version: the one place a
# version is set. To release, bump "version" there, commit, push main, wait for CI, and run:
#
#   scripts/release.sh <assets dir> [--dry-run]
#
# <assets dir> holds the upstream source tarballs of the vendored libheif, libde265 and dav1d, at
# the versions crates/heif-sys/vendor/VENDOR.txt records (FD14: every release carries them, so the
# LGPL's corresponding source stays available here). A re-vendor leaves them in vcpkg's
# downloads/; while the stack is unchanged, the previous release has them:
#   gh release download v<previous> --pattern '*.tar.gz' --dir <assets dir>
#
# The script checks before it does anything: main, clean, pushed, CI green on this commit, the
# tag new, both targets' libraries present and one tarball per library. Then it tags v<version>,
# pushes the tag, and creates the GitHub release with the tarballs, the committed vcpkg triplets
# and a SHA256SUMS.txt. --dry-run stops before tagging and prints the release notes.
#
# Bash on macOS, and Git Bash on Windows. Needs git and gh (signed in).

set -euo pipefail

die() { echo "release: $*" >&2; exit 1; }

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

assets=${1:-}
dry_run=no
[ "${2:-}" = "--dry-run" ] && dry_run=yes
[ -n "$assets" ] && [ -d "$assets" ] || die "usage: scripts/release.sh <assets dir> [--dry-run]"

# --- What product.json says -------------------------------------------------------------------
# product.json keeps a fixed, flat shape, so a line match is enough; build.rs parses it properly.
field() {
    sed -n "s/^[[:space:]]*\"$1\"[[:space:]]*:[[:space:]]*\"\(.*\)\"[[:space:]]*,\{0,1\}[[:space:]]*\$/\1/p" \
        product.json | head -n 1
}
name=$(field productName)
version=$(field version)
description=$(field description)
homepage=$(field homepage)
[ -n "$name" ] && [ -n "$version" ] && [ -n "$homepage" ] ||
    die "product.json needs productName, version and homepage"
[[ $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] ||
    die "product.json's version must be MAJOR.MINOR.PATCH, not '$version'"
tag="v$version"

# --- The checks -------------------------------------------------------------------------------
command -v gh >/dev/null || die "gh (GitHub CLI) is needed"
[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "release from main"
git diff --quiet && git diff --cached --quiet || die "commit or stash your changes first"
git fetch -q origin main --tags
head=$(git rev-parse HEAD)
[ "$head" = "$(git rev-parse origin/main)" ] || die "push main first: HEAD isn't origin/main"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then die "$tag already exists"; fi
if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null; then
    die "$tag already exists on origin"
fi

ci=$(gh run list --commit "$head" --workflow CI --json status,conclusion \
    --jq '.[0] | "\(.status) \(.conclusion)"' 2>/dev/null || true)
[ "$ci" = "completed success" ] || die "CI on ${head:0:7} isn't green (${ci:-no run}); wait for it"

for lib in crates/heif-sys/vendor/arm64-macos/lib/libheif.a \
           crates/heif-sys/vendor/x64-windows/lib/heif.lib; do
    [ -f "$lib" ] || die "$lib is missing: both targets' libraries ship in every release"
done

# The versions VENDOR.txt records for each target, and one tarball each.
vendored() {
    awk -v lib="$1" '$2 == lib && $3 ~ /^[0-9]+\.[0-9]+\.[0-9]+$/ { print $3 }' \
        crates/heif-sys/vendor/VENDOR.txt | sort -u
}
files=()
stack=""
for lib in libheif libde265 dav1d; do
    v=$(vendored "$lib")
    [ -n "$v" ] || die "VENDOR.txt records no version of $lib"
    [ "$(echo "$v" | wc -l)" -eq 1 ] || die "VENDOR.txt records two versions of $lib: $(echo $v)"
    shopt -s nullglob
    found=("$assets"/*"$lib"*"$v".tar.gz)
    shopt -u nullglob
    [ "${#found[@]}" -eq 1 ] || die "want one $lib $v source tarball in $assets, found ${#found[@]}"
    files+=("${found[0]}")
    stack="$stack${stack:+, }$lib $v"
done
for triplet in crates/heif-sys/vendor/triplets/*.cmake; do files+=("$triplet"); done

# --- The release ------------------------------------------------------------------------------
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/upload"
for f in "${files[@]}"; do cp "$f" "$work/upload/"; done
sums() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd "$work/upload" && sums -- *) > "$work/SHA256SUMS.txt"
mv "$work/SHA256SUMS.txt" "$work/upload/"
uploads=("$work"/upload/*)
notes="$work/notes.md"
{
    echo "$description."
    echo
    echo "**The vendored HEIF stack:** $stack, static, for arm64 macOS and x64 Windows (\`/MT\`)."
    echo "Their recipe and checks are in \`crates/heif-sys/vendor/VENDOR.txt\`."
    echo
    echo "**Attached** (FD14): the upstream source tarballs of those three libraries and the vcpkg"
    echo "overlay triplets they were built with, with \`SHA256SUMS.txt\`. libheif and libde265 are"
    echo "LGPL-3.0-or-later; \`THIRD-PARTY-NOTICES.md\` says how to relink an app against your own."
    echo
    echo "**Use it:**"
    echo
    echo '```toml'
    echo "fire-decode = { git = \"$homepage\", tag = \"$tag\", features = [\"psd\", \"heif\"] }"
    echo '```'
} > "$notes"

echo "== $name $version ($tag) at ${head:0:7}, CI green"
echo "== attaching:"
for f in "${uploads[@]}"; do echo "   $(basename "$f")"; done
echo "== notes:"
cat "$notes"
if [ "$dry_run" = yes ]; then
    echo "== --dry-run: no tag, no release"
    exit 0
fi

git tag -a "$tag" -m "$name $version"
git push origin "$tag"
gh release create "$tag" --verify-tag --title "$name $version" --notes-file "$notes" \
    "${uploads[@]}"
echo "== released $tag: $homepage/releases/tag/$tag"
