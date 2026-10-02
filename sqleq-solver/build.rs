// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Points the linker at a pre-built `libz3.so`.
//!
//! Against a pre-built library `z3-sys` emits no search path unless pkg-config finds one, and the
//! ambient linker search path isn't set up for it by default. And per the same reasoning as
//! `sqleq-fuzz/build.rs`'s rpath: a *dependency's* build-script link-arg doesn't reach the final
//! binary, so both the search path and the rpath have to be emitted from a crate that's actually
//! in this build's graph -- that's here, not `z3-sys`.
//!
//! `SQLEQ_Z3_LIB_DIR` has no default on purpose: the right path is machine-specific, and this
//! repo's rule for public/committed config is no machine-specific defaults -- fail loudly asking
//! for the env var instead of silently baking one in.

fn main() {
    println!("cargo:rerun-if-env-changed=SQLEQ_Z3_LIB_DIR");
    let lib_dir = std::env::var("SQLEQ_Z3_LIB_DIR").expect(
        "SQLEQ_Z3_LIB_DIR must point at the directory containing libz3.so (no default -- the \
         path is machine-specific).",
    );
    println!("cargo:rustc-link-search=native={lib_dir}");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "linux" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
    }
}
