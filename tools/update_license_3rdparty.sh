#!/bin/sh
# Regenerate LICENSE-3rdparty.csv, or with --check, fail if the committed file is stale.
#
# Crate rows come from dd-rust-license-tool, run once per workspace member: the root manifest is
# both the workspace and the sqleq-frontend package, so a single run walks only the frontend's
# dependencies and silently leaves out everything sqleq-fuzz pulls in. The tool also has no way to
# list a component that is not a crate, so those rows live in tools/license-3rdparty-extra.csv and
# are appended as-is.
set -eu

usage="usage: sh tools/update_license_3rdparty.sh [--check]"
check=0
case "${1-}" in
    "") ;;
    --check) check=1 ;;
    *) echo "$usage" >&2; exit 2 ;;
esac

command -v dd-rust-license-tool >/dev/null 2>&1 || {
    echo "dd-rust-license-tool not found; install it with:" >&2
    echo "  cargo install dd-rust-license-tool --version 1.0.6 --locked" >&2
    exit 1
}

cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# One file per manifest rather than a pipeline, so a failing run stops the script instead of
# vanishing into `sort`.
dd-rust-license-tool --manifest-path Cargo.toml dump >"$tmp/frontend.csv"
dd-rust-license-tool --manifest-path sqleq-fuzz/Cargo.toml dump >"$tmp/fuzz.csv"

{
    echo "Component,Origin,License,Copyright"
    { tail -n +2 "$tmp/frontend.csv"; tail -n +2 "$tmp/fuzz.csv"; } | LC_ALL=C sort -u
    tail -n +2 tools/license-3rdparty-extra.csv
} >"$tmp/new.csv"

if [ "$check" = 1 ]; then
    if ! diff -u LICENSE-3rdparty.csv "$tmp/new.csv"; then
        echo "LICENSE-3rdparty.csv is stale; regenerate it with: sh tools/update_license_3rdparty.sh" >&2
        exit 1
    fi
else
    cat "$tmp/new.csv" >LICENSE-3rdparty.csv
fi
