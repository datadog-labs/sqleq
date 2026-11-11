// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The build steps of the two features that link a native library; with neither, it does nothing.
//!
//! For `fuzz`: tells the executables where to find `libduckdb` at run time, and fetches the
//! PostgreSQL they run pairs on. For `cvc5`: puts the cvc5 library's directory in their rpath.
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
//!
//! PostgreSQL comes as a prebuilt archive from
//! [theseus-rs/postgresql-binaries](https://github.com/theseus-rs/postgresql-binaries), built from
//! the PostgreSQL project's own sources, for the platforms listed in [`PG_ARCHIVES`]. Each archive
//! is checked against the SHA-256 its release publishes, pinned here, and unpacked under
//! `target/<profile>/postgresql/`; the executables find it there, or beside themselves when they are
//! copied with it (`sqleq::fuzz::pg::bin_dir`). `SQLEQ_PG_DOWNLOAD=0` skips the download, for an
//! offline build. Nothing here fails the build: without a PostgreSQL it can use, `sqleq-fuzz` takes
//! the one `SQLEQ_PG_BIN` names or the one on `PATH`.
//!
//! The cvc5 library is linked dynamically from `CVC5_LIB_DIR` (see `cvc5-sys`), so the binary and
//! the tests must find it at run time too: that directory goes into their rpath. Without the
//! variable nothing is added, and cvc5 is found wherever the system linker looks. A directory that
//! does not exist is not added either -- `.cargo/config.toml` names one under `target/` whether or
//! not `tools/build_cvc5.sh` has run -- and the build says so, since the binary will then load
//! whichever cvc5 the loader finds, if any.

#[cfg(feature = "fuzz")]
use std::io::Read;
#[cfg(feature = "fuzz")]
use std::path::Path;

/// The PostgreSQL release fetched.
#[cfg(feature = "fuzz")]
const PG_VERSION: &str = "17.11.0";

/// Each platform's archive, by Rust target triple, and its SHA-256 as the release publishes it.
#[cfg(feature = "fuzz")]
const PG_ARCHIVES: &[(&str, &str)] = &[
    (
        "x86_64-unknown-linux-gnu",
        "b7a1ba6bae6499d8296e3e81b0171eecfd1766ca9aaa0057e41ad3e844e5e2e0",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "abffda09209280ec1502b73720dc4d254fb7fff9a072e324926c600a5b16c221",
    ),
    (
        "x86_64-apple-darwin",
        "e43a81b15e1cfe7f9d8fd79c6d4d0366e9001a5f690e322224dca704656602f7",
    ),
    (
        "aarch64-apple-darwin",
        "fd4b62794b160e26973a768a1eef3248aef9d2ff23ebd6d884a4299485e28e57",
    ),
];

/// More than any archive here, and a bound on what a misbehaving server can make the build read.
#[cfg(feature = "fuzz")]
const MAX_ARCHIVE_BYTES: u64 = 64 << 20;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "fuzz")]
    fuzz();
    #[cfg(feature = "cvc5")]
    cvc5();
}

#[cfg(feature = "fuzz")]
fn fuzz() {
    println!("cargo:rerun-if-env-changed=SQLEQ_PG_DOWNLOAD");
    // DuckDB's library sits beside the executables only when the `duckdb` feature links it.
    if std::env::var_os("CARGO_FEATURE_DUCKDB").is_some() {
        match std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default().as_str() {
            "linux" => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/deps:$ORIGIN"),
            "macos" => {
                println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/deps,-rpath,@loader_path");
            }
            _ => {}
        }
    }
    match fetch_postgres() {
        Ok(Some(rel)) => println!("cargo:rustc-env=SQLEQ_PG_FETCHED={rel}"),
        Ok(None) => {}
        Err(e) => println!(
            "cargo:warning=sqleq-fuzz: no PostgreSQL fetched ({e}); it will use the one \
             SQLEQ_PG_BIN names, or the one on PATH"
        ),
    }
}

/// Fetch and unpack this target's PostgreSQL unless it is already in place, and return its `bin`
/// directory relative to `target/<profile>/`. `None` when the download is switched off.
#[cfg(feature = "fuzz")]
fn fetch_postgres() -> Result<Option<String>, String> {
    if std::env::var("SQLEQ_PG_DOWNLOAD").as_deref() == Ok("0") {
        return Ok(None);
    }
    // A build of the published crate, which `cargo package` marks with `.cargo_vcs_info.json`, is
    // most often a `cargo install`, and cargo deletes that build's target directory when it is done:
    // a PostgreSQL fetched into it would be gone before the binary first ran.
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    if manifest.join(".cargo_vcs_info.json").is_file() {
        println!(
            "cargo:warning=sqleq-fuzz: no PostgreSQL is fetched for a build of the published crate; \
             put a PostgreSQL 17 on PATH, or set SQLEQ_PG_BIN to its bin directory"
        );
        return Ok(None);
    }
    let target = std::env::var("TARGET").map_err(|e| e.to_string())?;
    let digest = PG_ARCHIVES
        .iter()
        .find(|(t, _)| *t == target)
        .map(|(_, d)| *d)
        .ok_or(format!("no prebuilt archive for {target}"))?;
    // OUT_DIR is target/<profile>/build/<package>-<hash>/out.
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let profile_dir = out.ancestors().nth(3).ok_or("unexpected OUT_DIR layout")?;
    let name = format!("postgresql-{PG_VERSION}-{target}");
    let rel = format!("postgresql/{name}/bin");
    let bin = profile_dir.join(&rel);
    if !bin.join("postgres").is_file() {
        let url = format!(
            "https://github.com/theseus-rs/postgresql-binaries/releases/download/{PG_VERSION}/{name}.tar.gz"
        );
        let archive = download(&url)?;
        let got = sha256_hex(&archive);
        if got != digest {
            return Err(format!("{url} has SHA-256 {got}, not the pinned {digest}"));
        }
        unpack(&archive, &profile_dir.join("postgresql"), &name)?;
    }
    // A Linux archive uses the system's own OpenSSL, libxml2, Kerberos, zstd and lz4; say so now,
    // rather than at the first pair, if this machine lacks one. A cross build cannot run it.
    if std::env::var("HOST").ok() == Some(target.clone()) {
        let run = std::process::Command::new(bin.join("postgres"))
            .arg("--version")
            .output()
            .map_err(|e| format!("cannot run the fetched postgres: {e}"))?;
        if !run.status.success() {
            let why = String::from_utf8_lossy(&run.stderr);
            println!(
                "cargo:warning=sqleq-fuzz: the fetched PostgreSQL cannot run here ({}); install \
                 the libraries it names, or set SQLEQ_PG_BIN",
                why.trim()
            );
        }
    }
    Ok(Some(rel))
}

#[cfg(feature = "fuzz")]
fn download(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_with_config()
        .limit(MAX_ARCHIVE_BYTES)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{url}: {e}"))?;
    Ok(bytes)
}

#[cfg(feature = "fuzz")]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Unpack `archive` into `dest/<name>`, by way of a staging directory renamed into place, so that a
/// build interrupted halfway leaves nothing that looks complete.
#[cfg(feature = "fuzz")]
fn unpack(archive: &[u8], dest: &Path, name: &str) -> Result<(), String> {
    let staging = dest.join(format!(".{name}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    tar.set_preserve_permissions(true);
    tar.unpack(&staging).map_err(|e| format!("unpacking {name}: {e}"))?;
    let unpacked = staging.join(name);
    if !unpacked.join("bin").join("postgres").is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("{name} holds no bin/postgres"));
    }
    let _ = std::fs::remove_dir_all(dest.join(name));
    let moved = std::fs::rename(&unpacked, dest.join(name)).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&staging);
    moved
}

#[cfg(feature = "cvc5")]
fn cvc5() {
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
