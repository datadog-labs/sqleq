// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Tells the executables where to find `libduckdb` at run time, and fetches the PostgreSQL they run
//! pairs on.
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
//! copied with it (`sqleq_fuzz::pg::bin_dir`). `SQLEQ_PG_DOWNLOAD=0` skips the download, for an
//! offline build. Nothing here fails the build: without a PostgreSQL it can use, `sqleq-fuzz` takes
//! the one `SQLEQ_PG_BIN` names or the one on `PATH`.

use std::io::Read;
use std::path::Path;

/// The PostgreSQL release fetched.
const PG_VERSION: &str = "17.11.0";

/// Each platform's archive, by Rust target triple, and its SHA-256 as the release publishes it.
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
const MAX_ARCHIVE_BYTES: u64 = 64 << 20;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SQLEQ_PG_DOWNLOAD");
    match std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default().as_str() {
        "linux" => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/deps:$ORIGIN"),
        "macos" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/deps,-rpath,@loader_path");
        }
        _ => {}
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
fn fetch_postgres() -> Result<Option<String>, String> {
    if std::env::var("SQLEQ_PG_DOWNLOAD").as_deref() == Ok("0") {
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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Unpack `archive` into `dest/<name>`, by way of a staging directory renamed into place, so that a
/// build interrupted halfway leaves nothing that looks complete.
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
