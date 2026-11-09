// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The cvc5 library is linked dynamically from `CVC5_LIB_DIR` (see `cvc5-sys`), so the binary and
//! the tests must find it at run time too: that directory goes into their rpath. Without the
//! variable nothing is added, and cvc5 is found wherever the system linker looks. A directory that
//! does not exist is not added either -- `.cargo/config.toml` names one under `target/` whether or
//! not `tools/build_cvc5.sh` has run -- and the build says so, since the binary will then load
//! whichever cvc5 the loader finds, if any.

fn main() {
    println!("cargo:rerun-if-env-changed=CVC5_LIB_DIR");
    let Ok(dir) = std::env::var("CVC5_LIB_DIR") else { return };
    if dir.is_empty() {
        return;
    }
    println!("cargo:rerun-if-changed={dir}");
    if std::path::Path::new(&dir).is_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    } else {
        println!(
            "cargo:warning=CVC5_LIB_DIR={dir} does not exist, so it is not in the rpath; run \
             `sh tools/build_cvc5.sh`, or set CVC5_LIB_DIR to a cvc5 install's lib directory"
        );
    }
}
