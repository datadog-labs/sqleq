// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! Finding each backend binary, and noticing when one was built from older sources.
//!
//! Every `discover_*` returns an absolute path: each case runs in its own working directory, where
//! a relative `--frontend target/debug/...` names nothing. An override or variable that names a
//! missing or non-executable file is an error, never a silent fallback to another binary.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::util::{abspath, is_exe, newest, which};

/// This repository's root: the directory above this crate's.
pub fn repo() -> &'static Path {
    static REPO: OnceLock<PathBuf> = OnceLock::new();
    REPO.get_or_init(|| {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        here.canonicalize().unwrap_or_else(|_| crate::util::normalize(&here))
    })
}

fn target_builds(name: &str) -> Vec<PathBuf> {
    ["release", "debug"].iter().map(|p| repo().join("target").join(p).join(name)).collect()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// An explicit choice -- a flag, then a variable -- that must name an executable.
fn explicit(name: &str, flag: Option<&str>, var: &str) -> Result<Option<String>, String> {
    let env = std::env::var(var).ok().filter(|v| !v.is_empty());
    match flag.map(str::to_string).filter(|v| !v.is_empty()).or(env) {
        Some(c) if is_exe(Path::new(&c)) => Ok(Some(path_str(&abspath(Path::new(&c))))),
        Some(c) => Err(format!("error: {name} not found or not executable at: {c}")),
        None => Ok(None),
    }
}

/// sqleq-frontend: explicit override -> `$SQLEQ_FRONTEND` -> PATH -> this repo's own build (the
/// newest of release and debug).
pub fn discover_frontend(flag: Option<&str>) -> Result<String, String> {
    if let Some(p) = explicit("sqleq-frontend", flag, "SQLEQ_FRONTEND")? {
        return Ok(p);
    }
    if let Some(p) = which("sqleq-frontend") {
        return Ok(path_str(&abspath(&p)));
    }
    if let Some(p) = newest(&target_builds("sqleq-frontend")) {
        return Ok(path_str(&p));
    }
    Err("error: could not find 'sqleq-frontend'. Build it with `cargo build --release`, put it on PATH, \
         or pass --frontend/$SQLEQ_FRONTEND."
        .to_string())
}

/// qed-prover: explicit override -> `$QED_PROVER` -> PATH -> the newest Nix-wrapped prover (it
/// carries z3 + cvc5 on its own PATH), useful when not inside the dev shell.
pub fn discover_prover(flag: Option<&str>) -> Result<String, String> {
    if let Some(p) = explicit("qed-prover", flag, "QED_PROVER")? {
        return Ok(p);
    }
    if let Some(p) = which("qed-prover") {
        return Ok(path_str(&abspath(&p)));
    }
    let store: Vec<PathBuf> = std::fs::read_dir("/nix/store")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("-qed-prover"))
        .map(|e| e.path().join("bin").join("qed-prover"))
        .collect();
    if let Some(p) = newest(&store) {
        return Ok(path_str(&p));
    }
    Err("error: could not find 'qed-prover'. Enter the QED Nix shell, or pass --prover/$QED_PROVER.".to_string())
}

/// sqleq-fuzz: explicit override -> `$SQLEQ_FUZZ` -> PATH -> this repo's own build.
pub fn discover_fuzz(flag: Option<&str>) -> Result<String, String> {
    if let Some(p) = explicit("sqleq-fuzz", flag, "SQLEQ_FUZZ")? {
        return Ok(p);
    }
    if let Some(p) = which("sqleq-fuzz").or_else(|| newest(&target_builds("sqleq-fuzz"))) {
        return Ok(path_str(&abspath(&p)));
    }
    Err("error: could not find 'sqleq-fuzz'. Build it with `cargo build --release -p sqleq-fuzz`, put it \
         on PATH, or pass --fuzz-bin/$SQLEQ_FUZZ."
        .to_string())
}

/// sqleq-lean: explicit override -> `$SQLEQ_LEAN` -> this repo's release, then debug, build. The
/// first that exists wins, resolved through symlinks.
pub fn discover_lean(flag: Option<&str>) -> Result<String, String> {
    let env = std::env::var("SQLEQ_LEAN").ok();
    let builds = target_builds("sqleq-lean");
    let cands = [flag.map(str::to_string), env, Some(path_str(&builds[0])), Some(path_str(&builds[1]))];
    for c in cands.into_iter().flatten().filter(|c| !c.is_empty()) {
        let p = Path::new(&c);
        if is_exe(p) {
            return Ok(path_str(&p.canonicalize().unwrap_or_else(|_| abspath(p))));
        }
    }
    Err("error: --lean needs the sqleq-lean binary: `cargo build --release -p sqleq-lean` (it also needs \
         a Lean toolchain on PATH), or pass --lean-bin / set $SQLEQ_LEAN."
        .to_string())
}

/// How to run the second opinion's driver: the command up to its positional arguments, where to
/// run it, and with what environment laid over ours. Both implementations take the same
/// `<jobs> <results> --timeout-ms=N` and write the same rows, so everything after the command is
/// shared.
#[derive(Clone, Debug)]
pub struct SsDriver {
    /// `sqleq-solver` or `jvm`.
    pub imp: String,
    pub cmd: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// What the header line names.
    pub location: String,
}

/// sqleq-solver: explicit override -> `$SQLEQ_SOLVER_BIN` -> PATH -> this repo's own build. No
/// JDK, no fork tree and no library path: the build compiles Z3 from source and links it in
/// statically.
pub fn discover_sqleq_solver(flag: Option<&str>) -> Result<SsDriver, String> {
    let found = match explicit("sqleq-solver", flag, "SQLEQ_SOLVER_BIN")? {
        Some(p) => p,
        None => match which("sqleq-solver").or_else(|| newest(&target_builds("sqleq-solver"))) {
            Some(p) => path_str(&abspath(&p)),
            None => {
                return Err("error: could not find 'sqleq-solver'. Build it with `cargo build --release -p \
                            sqleq-solver` (its first build compiles Z3, which needs cmake and a C++20 \
                            compiler), put it on PATH, or pass --sqleq-solver-bin/$SQLEQ_SOLVER_BIN."
                    .to_string())
            }
        },
    };
    Ok(SsDriver {
        imp: "sqleq-solver".into(),
        cmd: vec![found.clone()],
        cwd: repo().to_path_buf(),
        env: Vec::new(),
        location: found,
    })
}

/// The JVM driver (`tools/sqlsolver/IrDriver.java`) over the de-Calcited fork: the original
/// SQLSolver, kept as a cross-check of sqleq-solver.
pub fn discover_sqlsolver_jvm(tree_flag: Option<&str>) -> Result<SsDriver, String> {
    let (cp, tree) = discover_sqlsolver(tree_flag)?;
    let lib = path_str(&tree.join("lib"));
    Ok(SsDriver {
        imp: "jvm".into(),
        cmd: vec!["java".into(), format!("-Djava.library.path={lib}"), "-cp".into(), cp, "IrDriver".into()],
        cwd: tree.clone(),
        env: vec![("LD_LIBRARY_PATH".into(), lib)],
        location: path_str(&tree),
    })
}

/// Resolve the fork as (classpath, working directory).
///
/// Override -> `$SQLEQ_SQLSOLVER`, and one of the two must say where: a default that resolves on
/// one machine only fails as "no classes there" rather than as "you did not say where". There is
/// no jar to run: the fork is compiled with javac into `build/classes-javac` and driven against
/// `$SQLEQ_SQLSOLVER_DEPS`, the pristine fat jar exploded with `org/apache/calcite` removed, which
/// is also what keeps the Calcite removal honest -- a surviving reference could not resolve.
///
/// The working directory is not incidental. `sqlsolver.properties` and `sqlsolver_data/` are read
/// relative to the tree root, and the Z3 bindings are loaded from `lib/`, which needs both
/// `-Djava.library.path` and `LD_LIBRARY_PATH`: one for the JVM's own lookup and one for the
/// dependent `.so` the first one pulls in.
pub fn discover_sqlsolver(tree_flag: Option<&str>) -> Result<(String, PathBuf), String> {
    let root = tree_flag
        .map(str::to_string)
        .or_else(|| std::env::var("SQLEQ_SQLSOLVER").ok())
        .filter(|r| !r.is_empty())
        .ok_or("error: --sqlsolver-jvm needs the fork's location: pass --sqlsolver-tree DIR or set $SQLEQ_SQLSOLVER.")?;
    let tree = PathBuf::from(root);
    let classes = tree.join("build").join("classes-javac");
    if !classes.is_dir() {
        return Err(format!(
            "error: no SQLSolver fork classes at {}.\n       Build the fork, or point --sqlsolver-tree / \
             $SQLEQ_SQLSOLVER at a tree that has them.",
            classes.display()
        ));
    }
    let deps = std::env::var("SQLEQ_SQLSOLVER_DEPS").ok().filter(|d| !d.is_empty()).ok_or(
        "error: set $SQLEQ_SQLSOLVER_DEPS to the exploded fat jar; the fork's classpath is not derivable from the tree.",
    )?;
    if !Path::new(&deps).is_dir() {
        return Err(format!("error: no dependency directory at {deps}; set $SQLEQ_SQLSOLVER_DEPS to the exploded jar."));
    }
    if which("javac").is_none() || which("java").is_none() {
        return Err("error: --sqlsolver-jvm needs a JDK on PATH (javac and java).".to_string());
    }
    let cp = format!("{deps}:{}", classes.display());
    let driver = compile_driver(&cp)?;
    Ok((format!("{cp}:{}", driver.display()), tree))
}

/// Rebuild the bridge driver when its sources are newer than its class, so a stale translator can
/// never be paired with fresh jobs.
///
/// Staleness is keyed on the newest of `IrDriver.java` and `IrToRel.java` rather than on the entry
/// point alone: `IrToRel` is the file that imports one Calcite or the other, so it is the one that
/// actually differs between the pristine tree and the de-Calcited fork -- which is also why the
/// output directory is `out-fork/` and is not shared with `out/`.
fn compile_driver(cp: &str) -> Result<PathBuf, String> {
    let dir = repo().join("tools").join("sqlsolver");
    let srcs: Vec<PathBuf> = ["IrDriver", "IrToRel"].iter().map(|n| dir.join(format!("{n}.java"))).collect();
    let missing: Vec<String> = srcs.iter().filter(|f| !f.is_file()).map(|f| path_str(f)).collect();
    if !missing.is_empty() {
        return Err(format!("error: the bridge sources are missing: {}", missing.join(", ")));
    }
    let out = dir.join("out-fork");
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let newest_src = srcs.iter().filter_map(|s| mtime(s)).max();
    if let (Some(cls), Some(src)) = (mtime(&out.join("IrDriver.class")), newest_src) {
        if cls >= src {
            return Ok(out);
        }
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("error: {}: {e}", out.display()))?;
    let mut argv: Vec<String> =
        ["javac", "--release", "17", "-proc:none", "-cp", cp, "-d"].iter().map(|s| s.to_string()).collect();
    argv.push(path_str(&out));
    argv.extend(srcs.iter().map(|s| path_str(s)));
    let r = crate::proc::run(&argv, None, &[], None);
    if r.rc != 0 {
        return Err(format!("error: cannot compile the bridge driver:\n{}", r.err.trim()));
    }
    Ok(out)
}

/// The crate whose sources each binary is built from, for [`stale_build`].
fn crate_dir(binary_name: &str) -> Option<PathBuf> {
    let r = repo();
    match binary_name {
        "sqleq-frontend" => Some(r.to_path_buf()),
        "sqleq-fuzz" | "sqleq-solver" | "sqleq-lean" => Some(r.join(binary_name)),
        _ => None,
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Why a binary built in this repo's `target/` is older than what it was built from, or `None`.
///
/// A pin blessed against a stale build records what an older tree said, and the next fresh build
/// reports it as a regression nobody made. Only binaries under this repo's `target/` are judged; one
/// from anywhere else has no sources here to compare with.
pub fn stale_build(binary: &str) -> Option<String> {
    let path = Path::new(binary).canonicalize().ok()?;
    let target = repo().join("target").canonicalize().ok()?;
    path.strip_prefix(&target).ok()?;
    let krate = crate_dir(&path.file_name()?.to_string_lossy())?;
    let mut srcs = vec![repo().join("Cargo.lock"), krate.join("Cargo.toml")];
    rs_files(&krate.join("src"), &mut srcs);
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let newest = srcs.iter().filter(|f| f.is_file()).max_by_key(|f| mtime(f))?;
    if mtime(newest)? > mtime(&path)? {
        let rel = newest.strip_prefix(repo()).unwrap_or(newest);
        return Some(format!("{binary} is older than {}", rel.display()));
    }
    None
}
