#!/bin/sh
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

# Fetch, build and install the cvc5 the cvc5 axis is written against, then build sqleq-cvc5 against
# it: `sh tools/build_cvc5.sh`, once. The axis needs the bag reasoning of the `bags-algebra` branch
# of cvc5 (https://github.com/mudathirmahgoub/cvc5/pull/27), which no cvc5 release has yet, so this
# builds that branch, merged with cvc5's main, at the commit pinned below, under target/cvc5/:
#
#   target/cvc5/src      the cvc5 sources at the pinned commit (a shallow fetch)
#   target/cvc5/install  the library and headers, which .cargo/config.toml points CVC5_LIB_DIR at
#
# The first run takes a while: cvc5 downloads and builds CaDiCaL, SymFPU, LibPoly, MPFR and, where
# the system has none, GMP, which it installs beside itself, and compiles for some 5 minutes on 18
# cores (15 on 8). A later run with the same commit skips straight to the cargo build. After it,
# `cargo build --features cvc5` and `cargo test --features cvc5` need no environment, and
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

# The commit the axis was measured against, on `bags-algebra-partition`; bump it to take a newer
# one. That branch is `bags-algebra` merged with cvc5's main, where an empty table has no group
# (cvc5/cvc5#13151): the encoding of GROUP BY relies on that, so `bags-algebra` itself, or any cvc5
# before it, gives wrong answers. At this commit the keys and NOT NULL columns of tables are lemmas
# of cvc5's static learning, on by default, and so is the rule EQ_EMPTY_ARG, a map or a bag.setof
# that is empty when its argument is: as a rewrite, which the bag solver also applied to its own
# lemmas, it could answer sat with a model that check-models rejects. Its rewriter writes out the
# table.group and table.aggr of a table of known rows, a union of `(bag x c)` with c constant,
# where the row a filter keeps, `(bag x (ite p c 0))`, counts too (cvc5/cvc5#13196, and c445480c
# for the filter): the cvc5-bounded axis's tables are such unions, so GROUP BY and aggregates are
# evaluated over their rows. It has cvc5's main up
# to #13183 only: #13178 removes cvc5_term_get_string_value and cvc5_mk_string_from_wchar, which the
# Rust bindings (cvc5 0.6.1) call.
repo=${SQLEQ_CVC5_REPO:-https://github.com/mudathirmahgoub/cvc5.git}
commit=${SQLEQ_CVC5_COMMIT:-c445480cf9e6ded3176236fb383f45375dabb769}
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
    # The Rust bindings (cvc5 0.6.1) are written against cvc5 1.4.0, whose cvc5_mk_grammar takes the
    # solver. cvc5 has since moved it to the term manager (cvc5/cvc5#13087), and the bindings do not
    # compile against that declaration. sqleq-cvc5 never calls it: until the bindings follow, they
    # read the declaration they were written against.
    sed -i.orig 's/cvc5_mk_grammar(Cvc5TermManager\* tm,/cvc5_mk_grammar(Cvc5* cvc5,/' \
        "$prefix/include/cvc5/c/cvc5.h"
    echo "$commit" >"$stamp"
    echo "cvc5 installed in $prefix"
fi

if [ "$build_axis" = 1 ]; then
    CVC5_LIB_DIR=$prefix/lib cargo build --release --no-default-features --features cvc5 --bin sqleq-cvc5
    echo "built target/release/sqleq-cvc5; try: sqleq-check --cvc5 --expect report-only tests/pairs"
fi
