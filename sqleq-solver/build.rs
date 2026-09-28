// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Points the linker at a pre-built `libz3.so`.
//!
//! `z3-sys` only emits `cargo:rustc-link-search` when it builds Z3 itself (the `static-link-z3`
//! feature, which this crate doesn't enable); against a system/pre-built library it relies on the
//! ambient linker search path, which isn't set up by default. And per the same reasoning as
//! `sqleq-fuzz/build.rs`'s rpath: a *dependency's* build-script link-arg doesn't reach the final
//! binary, so both the search path and the rpath have to be emitted from a crate that's actually
//! in this build's graph -- that's here, not `z3-sys`.
//!
//! `SQLEQ_Z3_LIB_DIR` has no default on purpose: the right path is machine-specific (a Nix store
//! path here, something else elsewhere), and this repo's rule for public/committed config is no
//! machine-specific defaults -- fail loudly asking for the env var instead of silently baking one
//! in. `Z3_SYS_Z3_HEADER` is read directly by `z3-sys`'s own build script and needs to be set
//! alongside this one, pointing at a `z3.h` whose directory also has `z3_api.h` etc next to it
//! (bindgen resolves the `#include`s relatively).

fn main() {
    println!("cargo:rerun-if-env-changed=SQLEQ_Z3_LIB_DIR");
    let lib_dir = std::env::var("SQLEQ_Z3_LIB_DIR").expect(
        "SQLEQ_Z3_LIB_DIR must point at the directory containing libz3.so (no default -- the \
         path is machine-specific). Z3_SYS_Z3_HEADER must also be set, pointing at a matching \
         z3.h, for z3-sys's own build script.",
    );
    println!("cargo:rustc-link-search=native={lib_dir}");
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "linux" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
    }
}
