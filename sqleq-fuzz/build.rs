//! Tells the executables where to find `libduckdb` at run time.
//!
//! `libduckdb-sys` copies the library it fetched into `target/<profile>/deps` and emits an rpath
//! for it, but `cargo:rustc-link-arg` from a *dependency's* build script does not reach the final
//! executable — so without this both the binary and the test harness die at startup with
//! `libduckdb.so: cannot open shared object file`. Cargo happens to paper over the test case by
//! setting `LD_LIBRARY_PATH`, which is why `cargo test` passes while `./target/release/sqleq-fuzz`
//! does not.
//!
//! The lookup is relative to the executable rather than absolute, so a built tree can be moved:
//! binaries land in `target/<profile>/` and test harnesses in `target/<profile>/deps/`, which is
//! why both directories are listed.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    match std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default().as_str() {
        "linux" => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/deps:$ORIGIN"),
        "macos" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/deps,-rpath,@loader_path");
        }
        _ => {}
    }
}
