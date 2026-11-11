#!/usr/bin/env bash
# Move the decoder pins in the workspace Cargo.toml to the newest versions their ranges allow.
#
#   scripts/update-decoders.sh                 every pinned crate
#   scripts/update-decoders.sh exr image png   just these
#
# Every decoder is pinned exactly (`=`) in Cargo.toml's [workspace.dependencies], the one place its
# version is set, so every app links what this repository's CI tests. `cargo update` can't move an
# exact pin, so this loosens each chosen pin to its semver range (`=1.74.3` becomes `1.74.3`, which
# cargo reads as ^1.74.3), runs `cargo update` on those crates, and pins each again at the version
# Cargo.lock then holds. A new major version, or a new minor before 1.0, changes the API: edit that
# pin by hand and adapt the code.
#
# Cargo.toml and Cargo.lock must be committed first, so `git checkout Cargo.toml Cargo.lock` undoes
# a run. Afterwards: CI's test sets, the performance check, THIRD-PARTY-NOTICES.md if a native
# library moved (libwebp-sys, lcms2-sys), then a release (README, "Versions and releases").
#
# Bash on macOS, and Git Bash on Windows. PowerShell has no `bash` on its PATH; there it's
#   & "$env:ProgramFiles\Git\bin\bash.exe" scripts/update-decoders.sh

set -euo pipefail

die() { echo "update-decoders: $*" >&2; exit 1; }

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

git diff --quiet HEAD -- Cargo.toml Cargo.lock ||
    die "commit or stash Cargo.toml and Cargo.lock first, so a run can be undone"

# The CR strips are for Git for Windows, which checks text out with CRLF line ends.

# --- The pins: `name = "=X"` or `name = { version = "=X", ...` in [workspace.dependencies] -----
pins=$(tr -d '\r' < Cargo.toml |
    sed -n '/^\[workspace\.dependencies\]/,/^\[/s/^\([A-Za-z0-9_-]*\) = .*"=\([0-9][^"]*\)".*/\1 \2/p')
[ -n "$pins" ] || die "no exact pins in Cargo.toml's [workspace.dependencies]"

if [ $# -gt 0 ]; then
    chosen=""
    for name in "$@"; do
        line=$(printf '%s\n' "$pins" | awk -v n="$name" '$1 == n')
        [ -n "$line" ] || die "$name isn't pinned in Cargo.toml"
        chosen="$chosen$line"$'\n'
    done
    pins=${chosen%$'\n'}
fi

# --- Helpers ------------------------------------------------------------------------------------
# Edit [workspace.dependencies] in place with one sed expression (GNU and BSD sed alike).
edit_deps() {
    sed -e "/^\[workspace\.dependencies\]/,/^\[/$1" Cargo.toml > Cargo.toml.tmp
    mv Cargo.toml.tmp Cargo.toml
}
re() { printf '%s' "$1" | sed 's/[.]/\\./g'; }
# What cargo's caret keeps fixed: the leftmost non-zero part (1.74.3 -> 1, 0.25.10 -> 0.25).
line_of() {
    local v=${1%%[+-]*}
    local major=${v%%.*} rest=${v#*.}
    local minor=${rest%%.*} patch=${rest#*.}
    if [ "$major" != 0 ]; then echo "$major"
    elif [ "$minor" != 0 ]; then echo "0.$minor"
    else echo "0.0.$patch"; fi
}
# Every version of crate $1 that Cargo.lock holds.
locked() {
    tr -d '\r' < Cargo.lock |
        awk -v n="$1" '$0 == "name = \"" n "\"" { getline; sub(/^version = "/, ""); sub(/"$/, ""); print }'
}
names() { tr -d '\r' | sed -n 's/^name = "\(.*\)"$/\1/p' | sort -u; }

# --- Loosen, update, pin again ------------------------------------------------------------------
specs=()
while read -r name version; do
    edit_deps "s/^\($name = .*\)\"=$(re "$version")\"/\1\"$version\"/"
    specs+=(-p "$name@$version")
done <<< "$pins"

cargo update "${specs[@]}"

moved=0
while read -r name version; do
    new=""
    for v in $(locked "$name"); do
        [ "$(line_of "$v")" = "$(line_of "$version")" ] && new="$new $v"
    done
    new=${new# }
    case "$new" in
        "") die "Cargo.lock has no $name in the $(line_of "$version") line" ;;
        *" "*) die "Cargo.lock has more than one $name in the $(line_of "$version") line: $new" ;;
    esac
    edit_deps "s/^\($name = .*\)\"$(re "$version")\"/\1\"=$new\"/"
    if [ "$new" = "$version" ]; then
        printf '  %-18s %s\n' "$name" "$version"
    else
        printf '  %-18s %s -> %s\n' "$name" "$version" "$new"
        moved=$((moved + 1))
    fi
done <<< "$pins"

# The pins and the lockfile agree, or this fails.
cargo metadata --locked --format-version 1 > /dev/null

added=$(comm -13 <(git show HEAD:Cargo.lock | names) <(names < Cargo.lock))
if [ -n "$added" ]; then
    echo
    echo "New in Cargo.lock; pin any that decodes (Cargo.toml says which count):"
    printf '  %s\n' $added
fi
echo
if [ "$moved" -eq 0 ]; then
    echo "No pin moved."
else
    echo "$moved pins moved. Next: CI's test sets, the performance check, THIRD-PARTY-NOTICES.md if"
    echo "libwebp-sys or lcms2-sys moved, then product.json's version and scripts/release.sh."
fi
