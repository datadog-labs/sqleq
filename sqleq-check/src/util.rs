// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The small pieces of a scripting language's standard library the harness leans on: temporary
//! directories, `abspath`, `which`, an executable check.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A name nobody else is using: pid, a per-process counter and the clock.
fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{:x}{:x}{:08x}", std::process::id(), n, nanos)
}

/// Create a new file at `name(suffix)`, retrying on a collision.
pub fn create_unique(name: impl Fn(&str) -> PathBuf) -> std::io::Result<(PathBuf, File)> {
    loop {
        let p = name(&unique_suffix());
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(f) => return Ok((p, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Create a new directory `$TMPDIR/<prefix><unique>`.
pub fn make_temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    loop {
        let p = std::env::temp_dir().join(format!("{prefix}{}", unique_suffix()));
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// A temporary directory, removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> std::io::Result<TempDir> {
        make_temp_dir(prefix).map(TempDir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Lexical normalization: `.` dropped, `..` folded into its parent, symlinks left alone -- what
/// `os.path.normpath` does.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `os.path.abspath`: absolute and normalized, without resolving symlinks.
pub fn abspath(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        normalize(&std::env::current_dir().unwrap_or_default().join(p))
    }
}

/// A regular file this process may execute: `os.path.isfile(p) and os.access(p, os.X_OK)`.
pub fn is_exe(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let Ok(c) = CString::new(p.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// `shutil.which`: the first executable `name` on `$PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_exe(p))
}

/// The most recently modified of the executables among `paths`.
pub fn newest(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .iter()
        .filter(|p| is_exe(p))
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .cloned()
}

/// The last non-empty line -- for a one-line reason, which is the useful part of a multi-line Java
/// stack or a frontend refusal.
pub fn tail(text: &str) -> String {
    text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// Python truthiness of a JSON value, which is how the prover's `.result` flags were read.
pub fn truthy(v: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match v {
        Null => false,
        Bool(b) => *b,
        Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        String(s) => !s.is_empty(),
        Array(a) => !a.is_empty(),
        Object(o) => !o.is_empty(),
    }
}

/// Round to `digits` decimal places, for the timings the JSON report carries.
pub fn round(x: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    (x * m).round() / m
}
