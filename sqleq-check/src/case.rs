// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! One case through the frontend and the QED prover.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::inputs::{suffix, triviality_from_ir, triviality_from_text};
use crate::proc::run_cmd;
use crate::util::{tail, truthy};

// Status taxonomy.
/// The prover proved the two queries equivalent.
pub const PROVABLE: &str = "provable";
/// The prover ran but could not prove equivalence.
pub const UNPROVABLE: &str = "unprovable";
/// The frontend would not lower the SQL (see `refuse_kind`).
pub const REFUSED: &str = "refused";
/// The prover panicked / crashed on the case.
pub const PANIC: &str = "panic";
/// Exceeded the per-case wall-clock budget.
pub const TIMEOUT: &str = "timeout";
/// Anything else (e.g. JSON it couldn't read).
pub const ERROR: &str = "error";
/// Lowered, and the qed axis was not asked (see `--axes`).
pub const LOWERED: &str = "lowered";

pub const STATUS_ORDER: [&str; 7] = [PROVABLE, UNPROVABLE, TIMEOUT, REFUSED, PANIC, ERROR, LOWERED];

pub fn status_rank(status: &str) -> usize {
    STATUS_ORDER.iter().position(|s| *s == status).unwrap_or(99)
}

#[derive(Clone, Debug, Serialize)]
pub struct Case {
    /// Display name (path relative to a common root).
    pub name: String,
    /// The source path, as it was collected.
    pub path: String,
    pub status: String,
    /// Harness-measured total wall time (s).
    pub wall: f64,
    pub lower_wall: f64,
    pub prove_wall: f64,
    /// parse | unsupported | schema | parameter-misaligned
    pub refuse_kind: String,
    /// The frontend produced a plan (or the input was one).
    pub lowered: bool,
    pub complete_fragment: bool,
    pub smt_timed_out: bool,
    pub nontrivial_perms: bool,
    /// The two queries are the same; `None` = undetermined.
    pub trivial: Option<bool>,
    /// "ir" | "text" -- how `trivial` was decided.
    pub trivial_basis: String,
    /// The full prover Stats from `.result`.
    pub stats: Value,
    /// Error detail when not provable/unprovable.
    pub message: String,
    // The second opinion, only when one was asked for. `s_verdict` is that prover's raw label, kept
    // beside the bucket purely for audit: nothing may branch on it, because `NEQ` is not a
    // refutation.
    pub s_bucket: Option<String>,
    pub s_verdict: Option<String>,
    pub s_ms: Option<Value>,
    pub s_note: String,
    // The Lean axis, only when it is asked: sqleq-lean's own verdict on the pair file (it reads the
    // .sql itself, so it answers whatever the frontend did).
    pub l_verdict: Option<String>,
    pub l_reason: String,
    pub l_shape: String,
    pub l_ms: Option<Value>,
    // The sqleq-fuzz axis, only when `fuzz` is among --axes: its label's kind (the part before the
    // first `:`), the counterexample or the reason, and its wall time.
    pub f_verdict: Option<String>,
    pub f_note: String,
    pub f_ms: Option<u64>,
}

impl Case {
    pub fn new(name: &str, path: &str) -> Case {
        Case {
            name: name.to_string(),
            path: path.to_string(),
            status: ERROR.to_string(),
            wall: 0.0,
            lower_wall: 0.0,
            prove_wall: 0.0,
            refuse_kind: String::new(),
            lowered: false,
            complete_fragment: false,
            smt_timed_out: false,
            nontrivial_perms: false,
            trivial: None,
            trivial_basis: String::new(),
            stats: Value::Object(Default::default()),
            message: String::new(),
            s_bucket: None,
            s_verdict: None,
            s_ms: None,
            s_note: String::new(),
            l_verdict: None,
            l_reason: String::new(),
            l_shape: String::new(),
            l_ms: None,
            f_verdict: None,
            f_note: String::new(),
            f_ms: None,
        }
    }

    pub fn is_sql(&self) -> bool {
        self.path.ends_with(".sql")
    }
}

/// Map the frontend's stderr to (refuse_kind, one-line reason).
///
/// The four kinds mirror `FrontendError`: a `PARSE ERROR:` prefix means sqlparser rejected the
/// text, `unsupported:` means we declined to lower a construct, `parameter-misaligned:` means the
/// two queries' `$N` do not line up, and anything else is a schema/shape complaint (unresolved
/// column, wrong number of queries, bad DDL) -- which `Display` leaves unprefixed because those
/// messages are already self-describing.
pub fn classify_refusal(err: &str) -> (String, String) {
    let reason = tail(err);
    let reason = if reason.is_empty() { "frontend produced no JSON output".to_string() } else { reason };
    let kind = if reason.starts_with("PARSE ERROR") {
        "parse"
    } else if reason.starts_with("unsupported:") {
        "unsupported"
    } else if reason.starts_with("parameter-misaligned:") {
        "parameter-misaligned"
    } else {
        "schema"
    };
    (kind.to_string(), reason)
}

/// The frontend flags a `.sql` case's `-- catalog:` header asks for (none when absent).
///
/// A pair whose queries use `$N` needs an inferred catalog: under the default, declared one the
/// frontend refuses a bare placeholder. An unknown value is an error, rather than silently lowering
/// against a catalog the case did not ask for.
pub fn catalog_flags(src: &Path) -> Result<Vec<String>, String> {
    if suffix(src) != ".sql" {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let h = crate::suite::parse_header(&text);
    crate::suite::catalog_flags(&h.catalog)
        .map(|f| f.iter().map(|s| s.to_string()).collect())
        .ok_or_else(|| format!("unknown `catalog: {}`", h.catalog))
}

/// Record whether this case is `x` against `x`. Prefers the IR test and falls back to the source
/// text when there is no IR -- a refusal still has a triviality, and it is worth knowing whether
/// the refusals are landing on the pairs that would have counted.
pub fn set_triviality(case: &mut Case, plan_path: Option<&Path>, src: &Path) {
    if let Some(p) = plan_path {
        let verdict = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| triviality_from_ir(&v));
        if let Some(v) = verdict {
            case.trivial = Some(v);
            case.trivial_basis = "ir".into();
            return;
        }
    }
    if suffix(src) != ".json" {
        if let Some(v) = std::fs::read_to_string(src).ok().and_then(|t| triviality_from_text(&t)) {
            case.trivial = Some(v);
            case.trivial_basis = "text".into();
        }
    }
}

/// A case's display name may be a relative path; a job file's name may not.
pub fn ss_slug(name: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    if out.is_empty() {
        "case".into()
    } else {
        out
    }
}

/// Where a case runs: its own directory, kept under `--keep` or removed afterwards.
pub struct Workdir {
    pub path: PathBuf,
    cleanup: bool,
}

impl Workdir {
    pub fn new(keep_dir: Option<&Path>, name: &str) -> std::io::Result<Workdir> {
        match keep_dir {
            Some(k) => {
                let p = k.join(name.replace('/', "__"));
                std::fs::create_dir_all(&p)?;
                Ok(Workdir { path: p, cleanup: false })
            }
            None => Ok(Workdir { path: crate::util::make_temp_dir("sqleq-")?, cleanup: true }),
        }
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// What a case's prover stage needs besides the case itself.
#[derive(Clone, Debug, Default)]
pub struct Stage {
    pub frontend: String,
    pub prover: Option<String>,
    pub timeout: f64,
    pub smt_timeout_ms: Option<u64>,
    pub keep_dir: Option<PathBuf>,
    pub ss_dir: Option<PathBuf>,
}

fn s(v: &str) -> String {
    v.to_string()
}

/// The frontend, then the prover, on one case.
pub fn run_case(src: &Path, name: &str, st: &Stage) -> Case {
    let mut case = Case::new(name, &src.to_string_lossy());
    let mut t0 = Instant::now();
    let wd = match Workdir::new(st.keep_dir.as_deref(), name) {
        Ok(w) => w,
        Err(e) => {
            case.message = format!("cannot create a work directory: {e}");
            return case;
        }
    };
    let workdir = wd.path.clone();
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let json_name = format!("{stem}.json");
    let json_path = workdir.join(&json_name);

    if suffix(src) == ".json" {
        // Pre-parsed relational plan: skip the frontend stage entirely.
        if let Err(e) = std::fs::copy(src, &json_path) {
            case.message = format!("cannot copy {}: {e}", src.display());
            case.wall = t0.elapsed().as_secs_f64();
            return case;
        }
    } else {
        // 1) Lower SQL -> JSON.
        let sql_name = format!("{stem}.sql");
        if let Err(e) = std::fs::copy(src, workdir.join(&sql_name)) {
            case.message = format!("cannot copy {}: {e}", src.display());
            case.wall = t0.elapsed().as_secs_f64();
            return case;
        }
        let flags = match catalog_flags(src) {
            Ok(f) => f,
            Err(e) => {
                case.message = e;
                case.wall = t0.elapsed().as_secs_f64();
                return case;
            }
        };
        let mut argv = vec![st.frontend.clone()];
        argv.extend(flags);
        argv.extend([sql_name, json_name.clone()]);
        let fr = run_cmd(&argv, &workdir, st.timeout, &[]);
        case.lower_wall = fr.wall;
        if fr.timed_out {
            case.status = s(TIMEOUT);
            case.message = s("frontend timed out");
            set_triviality(&mut case, None, src);
            case.wall = t0.elapsed().as_secs_f64();
            return case;
        }
        // The frontend's exit code is meaningful, but check the artifact too: a zero exit with no
        // JSON is still a case we cannot prove, and silently proving nothing would be worse.
        let empty = std::fs::metadata(&json_path).map_or(true, |m| m.len() == 0);
        if fr.rc != 0 || empty {
            case.status = s(REFUSED);
            (case.refuse_kind, case.message) = classify_refusal(&fr.err);
            set_triviality(&mut case, None, src);
            case.wall = t0.elapsed().as_secs_f64();
            return case;
        }
    }

    case.lowered = true;
    set_triviality(&mut case, Some(&json_path), src);

    // 1b) Package the plan for the second opinion, while the workdir still exists. This is
    // `{name, ir, schema}` built from *this* JSON -- the same bytes the prover is about to read --
    // so nothing re-lowers the case and the two axes cannot drift apart. Built before the prover
    // runs, so a prover timeout does not also cost the second opinion; its own cost is discounted
    // from `case.wall` so a second-opinion run's timings stay comparable to one without it.
    if let Some(ss_dir) = &st.ss_dir {
        let job = ss_dir.join(format!("{}.job.jsonl", ss_slug(name)));
        let argv = [
            st.frontend.clone(),
            s("--sqlsolver"),
            s("--ir"),
            json_name.clone(),
            s("--name"),
            name.to_string(),
            s("-o"),
            job.to_string_lossy().into_owned(),
        ];
        let pack = run_cmd(&argv, &workdir, st.timeout, &[]);
        t0 += Duration::from_secs_f64(pack.wall);
        if pack.rc != 0 || !job.exists() {
            case.s_bucket = Some(s(crate::axes::solver::UNSUPPORTED));
            let why = tail(&pack.err);
            case.s_note = if why.is_empty() { format!("could not package the plan (exit {})", pack.rc) } else { why };
        }
    }

    let Some(prover) = &st.prover else {
        case.status = s(LOWERED);
        case.wall = t0.elapsed().as_secs_f64();
        return case;
    };

    // 2) Prove equivalence.
    let env: Vec<(String, String)> =
        st.smt_timeout_ms.map(|ms| vec![(s("QED_SMT_TIMEOUT"), ms.to_string())]).unwrap_or_default();
    let remaining = (st.timeout - case.lower_wall).max(1.0);
    let qr = run_cmd(&[prover.clone(), json_name], &workdir, remaining, &env);
    case.prove_wall = qr.wall;
    if qr.timed_out {
        case.status = s(TIMEOUT);
        case.message = s("prover timed out");
        case.wall = t0.elapsed().as_secs_f64();
        return case;
    }
    read_result(&mut case, &workdir.join(format!("{stem}.result")), &qr);
    case.wall = t0.elapsed().as_secs_f64();
    case
}

/// The prover's verdict: its `.result` file when it wrote one, else what it printed.
pub fn read_result(case: &mut Case, result_path: &Path, qr: &crate::proc::Run) {
    let stats = std::fs::read_to_string(result_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Default::default()));
    let flag = |k: &str| stats.get(k).is_some_and(truthy);
    if stats.as_object().is_some_and(|o| !o.is_empty()) {
        case.complete_fragment = flag("complete_fragment");
        case.smt_timed_out = flag("smt_timed_out");
        case.nontrivial_perms = flag("nontrivial_perms");
        if flag("panicked") {
            case.status = s(PANIC);
            case.message = s("prover panicked");
        } else if flag("provable") {
            case.status = s(PROVABLE);
        } else {
            case.status = s(UNPROVABLE);
        }
    } else if qr.out.contains("is provable for") && !qr.out.contains("is not provable for") {
        // No .result written: infer from stdout, else treat as panic/error.
        case.status = s(PROVABLE);
    } else if qr.out.contains("is not provable for") {
        case.status = s(UNPROVABLE);
    } else if qr.rc != 0 {
        case.status = s(PANIC);
        let text = if qr.err.is_empty() { &qr.out } else { &qr.err };
        let msg: String = text.trim().chars().take(400).collect();
        case.message = if msg.is_empty() { format!("prover exit {}", qr.rc) } else { msg };
    } else {
        case.status = s(ERROR);
        case.message = s("no .result and no verdict on stdout");
    }
    case.stats = stats;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_refusal_knows_all_four_kinds() {
        assert_eq!(classify_refusal("PARSE ERROR: x").0, "parse");
        assert_eq!(classify_refusal("unsupported: x").0, "unsupported");
        assert_eq!(classify_refusal("parameter-misaligned: arity").0, "parameter-misaligned");
        assert_eq!(classify_refusal("unresolved column x").0, "schema");
        assert_eq!(classify_refusal("").1, "frontend produced no JSON output");
    }

    #[test]
    fn slugs_name_files() {
        assert_eq!(ss_slug("dir/a b.sql"), "dir_a_b.sql");
        assert_eq!(ss_slug("a//b"), "a_b");
        assert_eq!(ss_slug(""), "case");
    }
}
