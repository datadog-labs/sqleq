// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The cvc5 library is linked dynamically from `CVC5_LIB_DIR` (see `cvc5-sys`), so the binary and
//! the tests must find it at run time too: that directory goes into their rpath. Without the
//! variable nothing is added, and cvc5 is found wherever the system linker looks.

fn main() {
    println!("cargo:rerun-if-env-changed=CVC5_LIB_DIR");
    if let Ok(dir) = std::env::var("CVC5_LIB_DIR") {
        if !dir.is_empty() {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
        }
    }
}
