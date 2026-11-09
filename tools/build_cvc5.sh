#!/bin/sh
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

# Fetch, build and install the cvc5 the cvc5 axis is written against, then build sqleq-cvc5 against
# it: `sh tools/build_cvc5.sh`, once. The axis needs the bag reasoning of the `bags-algebra` branch
# of cvc5 (https://github.com/mudathirmahgoub/cvc5/pull/27), which no cvc5 release has yet, so this
# builds that branch at the commit pinned below, under target/cvc5/:
#
#   target/cvc5/src      the cvc5 sources at the pinned commit (a shallow fetch)
#   target/cvc5/install  the library and headers, which .cargo/config.toml points CVC5_LIB_DIR at
#
# The first run takes a while: cvc5 downloads and builds CaDiCaL, SymFPU, LibPoly, MPFR and, where
# the system has none, GMP, which it installs beside itself, and compiles for some 5 minutes on 18
# cores (15 on 8). A later run with the same commit skips straight to the cargo build. After it,
# `cargo build -p sqleq-cvc5` and `cargo test -p sqleq-cvc5` need no environment, and
# `sqleq-check --cvc5` finds the binary in target/release.
#
# Needs git, cmake 3.16 or later, a C++17 compiler, Python 3 (with the tomli package below 3.11) and
# libclang, which the cvc5 bindings generate their FFI with.
#
#   --cvc5-only           build and install cvc5, but not sqleq-cvc5
#   SQLEQ_CVC5_REPO       the repository to fetch from (default: the branch's fork)
#   SQLEQ_CVC5_COMMIT     the commit to build (default: the pinned one)
#   SQLEQ_CVC5_JOBS       parallel compile jobs (default: the number of CPUs)
set -eu

usage="usage: sh tools/build_cvc5.sh [--cvc5-only]"
build_axis=1
case "${1-}" in
    "") ;;
    --cvc5-only) build_axis=0 ;;
    *) echo "$usage" >&2; exit 2 ;;
esac

# The tip of `bags-algebra` that the axis was measured against; bump it to take a newer one.
repo=${SQLEQ_CVC5_REPO:-https://github.com/mudathirmahgoub/cvc5.git}
commit=${SQLEQ_CVC5_COMMIT:-249a95c03976fb99483fef46cb16adf75ab78aef}
jobs=${SQLEQ_CVC5_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}

for tool in git cmake python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "build_cvc5.sh: $tool not found" >&2; exit 1; }
done

cd "$(dirname "$0")/.."
root=$(pwd)
src=$root/target/cvc5/src
prefix=$root/target/cvc5/install
stamp=$prefix/.sqleq-cvc5-commit

if [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$commit" ]; then
    echo "cvc5 $commit is already installed in $prefix"
else
    echo "fetching cvc5 $commit from $repo"
    rm -rf "$src" "$prefix"
    mkdir -p "$src"
    git -C "$src" init -q
    git -C "$src" fetch -q --depth 1 "$repo" "$commit"
    git -C "$src" checkout -q --detach FETCH_HEAD

    echo "building cvc5 with $jobs jobs (log in target/cvc5/build.log)"
    (
        cd "$src"
        ./configure.sh unrestricted --auto-download --prefix="$prefix"
        cmake --build build -j "$jobs"
        cmake --install build
    ) >"$root/target/cvc5/build.log" 2>&1 || {
        tail -n 30 "$root/target/cvc5/build.log" >&2
        echo "build_cvc5.sh: the cvc5 build failed; see target/cvc5/build.log" >&2
        exit 1
    }
    echo "$commit" >"$stamp"
    echo "cvc5 installed in $prefix"
fi

if [ "$build_axis" = 1 ]; then
    CVC5_LIB_DIR=$prefix/lib cargo build --release -p sqleq-cvc5
    echo "built target/release/sqleq-cvc5; try: sqleq-check --cvc5 --expect report-only tests/pairs"
fi
