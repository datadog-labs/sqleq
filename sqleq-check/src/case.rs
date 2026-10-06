// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! One case through the frontend and the QED prover.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::inputs::{first_row_name, suffix, triviality_from_ir, triviality_from_text, CorpusRow, Item};
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
    /// The combined verdict, only under `--portfolio`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<crate::portfolio::Outcome>,
    /// A refused case whose two sides normalize to the same query: settled by reflexivity without
    /// being lowered. The frontend says so in its corpus report's `reflexive` status, and for a
    /// single pair in a [`sqleq_frontend::REFLEXIVE_NOTE`] line ahead of the refusal.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reflexive: bool,
    /// Each backend's own record, unbucketed: the fuzz label with its partial-trial count, the
    /// solver's row, the Lean record. Kept for a consumer that needs more than the bucket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f_raw: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s_raw: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l_raw: Option<Value>,
    /// The end of the prover's stdout and stderr when it crashed: the signature a crash is told
    /// apart by.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub q_tail: String,
    /// The prover's exit code when it crashed or panicked, minus the signal when a signal ended it:
    /// `-11` is the z3 null dereference, which only the code tells apart from other crashes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q_rc: Option<i32>,
    /// The corpus row this case is, when it is one.
    #[serde(skip)]
    pub row: Option<CorpusRow>,
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
            portfolio: None,
            reflexive: false,
            f_raw: None,
            s_raw: None,
            l_raw: None,
            q_tail: String::new(),
            q_rc: None,
            row: None,
        }
    }

    pub fn of(item: &Item) -> Case {
        let mut c = Case::new(&item.name, &item.path.to_string_lossy());
        c.row = item.row.clone();
        c
    }

    pub fn is_sql(&self) -> bool {
        self.path.ends_with(".sql")
    }

    /// Whether there is a pair for the axes that read it themselves -- fuzz and Lean: a `.sql`
    /// file, or a corpus row. A pre-lowered `.json` plan has none.
    pub fn has_pair(&self) -> bool {
        self.row.is_some() || self.is_sql()
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
///
/// `default` is `--catalog`'s choice, for a case whose header names none: a header always wins,
/// because it states what that one pair needs.
pub fn catalog_flags(src: &Path, default: Option<&str>) -> Result<Vec<String>, String> {
    let named = |c: &str| {
        crate::suite::catalog_flags(c)
            .map(|f| f.iter().map(|s| s.to_string()).collect())
            .ok_or_else(|| format!("unknown `catalog: {c}`"))
    };
    if suffix(src) != ".sql" {
        return named(default.unwrap_or("declared"));
    }
    let text = std::fs::read_to_string(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let h = crate::suite::parse_header(&text);
    if h.lines.contains_key("catalog") {
        named(&h.catalog)
    } else {
        named(default.unwrap_or("declared"))
    }
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
    /// `--catalog`: the catalog a case is lowered against when its header names none.
    pub catalog: Option<String>,
    /// `--qed-mem-gib`, in bytes.
    pub qed_mem: Option<u64>,
}

fn s(v: &str) -> String {
    v.to_string()
}

/// The frontend, then the prover, on one case.
pub fn run_case(item: &Item, st: &Stage) -> Case {
    let mut case = Case::of(item);
    let (src, name) = (item.path.as_path(), item.name.as_str());
    let mut t0 = Instant::now();
    let wd = match Workdir::new(st.keep_dir.as_deref(), name) {
        Ok(w) => w,
        Err(e) => {
            case.message = format!("cannot create a work directory: {e}");
            return case;
        }
    };
    let Some(json_name) = lower(&mut case, src, &wd.path, &st.frontend, st.catalog.as_deref(), st.timeout) else {
        case.wall = t0.elapsed().as_secs_f64();
        return case;
    };

    // 1b) Package the plan for the second opinion, while the workdir still exists. Built before the
    // prover runs, so a prover timeout does not also cost the second opinion; its own cost is
    // discounted from `case.wall` so a second-opinion run's timings stay comparable to one without
    // it.
    if let Some(ss_dir) = &st.ss_dir {
        let job = ss_dir.join(format!("{}.job.jsonl", ss_slug(name)));
        t0 += Duration::from_secs_f64(package(&mut case, &st.frontend, &wd.path, &json_name, &job, st.timeout));
    }

    let Some(prover) = &st.prover else {
        case.status = s(LOWERED);
        case.wall = t0.elapsed().as_secs_f64();
        return case;
    };
    let remaining = (st.timeout - case.lower_wall).max(1.0);
    prove(&mut case, &wd.path, &json_name, prover, remaining, st.smt_timeout_ms, st.qed_mem);
    case.wall = t0.elapsed().as_secs_f64();
    case
}

/// The first stage: lower SQL -> JSON in `workdir`, or take a pre-parsed plan as it is. Returns the
/// plan's file name when there is one to prove, and `None` when the case is already decided
/// (refused, timed out, or an error), with its status set.
pub fn lower(
    case: &mut Case,
    src: &Path,
    workdir: &Path,
    frontend: &str,
    catalog: Option<&str>,
    timeout: f64,
) -> Option<String> {
    if let Some(row) = case.row.clone() {
        return lower_row(case, &row, workdir, frontend, catalog, timeout);
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let json_name = format!("{stem}.json");
    let json_path = workdir.join(&json_name);

    if suffix(src) == ".json" {
        // Pre-parsed relational plan: skip the frontend stage entirely.
        if let Err(e) = std::fs::copy(src, &json_path) {
            case.message = format!("cannot copy {}: {e}", src.display());
            return None;
        }
    } else {
        let sql_name = format!("{stem}.sql");
        if let Err(e) = std::fs::copy(src, workdir.join(&sql_name)) {
            case.message = format!("cannot copy {}: {e}", src.display());
            return None;
        }
        let flags = match catalog_flags(src, catalog) {
            Ok(f) => f,
            Err(e) => {
                case.message = e;
                return None;
            }
        };
        let mut argv = vec![frontend.to_string()];
        argv.extend(flags);
        argv.extend([sql_name, json_name.clone()]);
        let fr = run_cmd(&argv, workdir, timeout, &[]);
        case.lower_wall = fr.wall;
        if fr.timed_out {
            case.status = s(TIMEOUT);
            case.message = s("frontend timed out");
            set_triviality(case, None, src);
            return None;
        }
        // The frontend's exit code is meaningful, but check the artifact too: a zero exit with no
        // JSON is still a case we cannot prove, and silently proving nothing would be worse.
        let empty = std::fs::metadata(&json_path).map_or(true, |m| m.len() == 0);
        if fr.rc != 0 || empty {
            case.status = s(REFUSED);
            (case.refuse_kind, case.message) = classify_refusal(&fr.err);
            case.reflexive = fr.err.lines().any(|l| l.trim() == sqleq_frontend::REFLEXIVE_NOTE);
            set_triviality(case, None, src);
            return None;
        }
    }
    case.lowered = true;
    set_triviality(case, Some(&json_path), src);
    Some(json_name)
}

/// Lower a corpus row through the frontend's own corpus mode, handed the row as a one-row CSV, so it
/// is read, lowered and reported exactly as that row of the whole file would be -- its DDL through
/// the corpus mode's lenient reader, not the stricter one a `.sql` file's goes through. The report
/// says `emit`, `refuse` or `reflexive` (refused, but settled by reflexivity), with the refusal's
/// kind and reason.
fn lower_row(
    case: &mut Case,
    row: &CorpusRow,
    workdir: &Path,
    frontend: &str,
    catalog: Option<&str>,
    timeout: f64,
) -> Option<String> {
    let flags = match crate::suite::catalog_flags(catalog.unwrap_or("declared")) {
        Some(f) => f.iter().map(|s| s.to_string()),
        None => {
            case.message = format!("unknown catalog `{}`", catalog.unwrap_or_default());
            return None;
        }
    };
    let csv = match row.write_csv(workdir) {
        Ok(p) => p,
        Err(e) => {
            case.message = format!("cannot write the row: {e}");
            return None;
        }
    };
    let mut argv = vec![frontend.to_string()];
    argv.extend(flags);
    argv.extend([
        "--csv".to_string(),
        csv.to_string_lossy().into_owned(),
        "-o".into(),
        "lowered".into(),
        "--report".into(),
        "report.json".into(),
    ]);
    let fr = run_cmd(&argv, workdir, timeout, &[]);
    case.lower_wall = fr.wall;
    let text_trivial = |case: &mut Case| {
        let (a, b) = (&row.0.a, &row.0.b);
        if let Some(v) = triviality_from_text(&format!("{a};\n{b};")) {
            (case.trivial, case.trivial_basis) = (Some(v), "text".into());
        }
    };
    if fr.timed_out {
        case.status = s(TIMEOUT);
        case.message = s("frontend timed out");
        text_trivial(case);
        return None;
    }
    let report: Option<Value> =
        std::fs::read_to_string(workdir.join("report.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
    let Some(detail) = report.as_ref().and_then(|r| r.pointer("/detail/0")) else {
        // No report is a frontend that died, not a refusal: there is no reason to give.
        case.message = {
            let why = tail(&fr.err);
            if why.is_empty() { format!("frontend exit {} with no report", fr.rc) } else { why }
        };
        text_trivial(case);
        return None;
    };
    let field = |k: &str| detail.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match field("status").as_str() {
        "emit" => {
            let json_name = format!("{}.json", case.name);
            let json_path = workdir.join(&json_name);
            let lowered = workdir.join("lowered").join(format!("{}.json", first_row_name()));
            if let Err(e) = std::fs::rename(lowered, &json_path) {
                case.message = format!("the frontend reported a plan it did not write: {e}");
                return None;
            }
            case.lowered = true;
            set_triviality(case, Some(&json_path), Path::new(""));
            Some(json_name)
        }
        status => {
            case.status = s(REFUSED);
            case.refuse_kind = field("kind");
            case.message = field("reason");
            case.reflexive = status == "reflexive";
            text_trivial(case);
            None
        }
    }
}

/// Package the lowered plan as the second opinion's job at `job`: `{name, ir, schema}` built from
/// *this* JSON -- the same bytes the prover reads -- so nothing re-lowers the case and the two axes
/// cannot drift apart. A plan the bridge cannot express is `unsupported` on the case. Returns the
/// wall time it took.
pub fn package(case: &mut Case, frontend: &str, workdir: &Path, json_name: &str, job: &Path, timeout: f64) -> f64 {
    let argv = [
        frontend.to_string(),
        s("--sqlsolver"),
        s("--ir"),
        json_name.to_string(),
        s("--name"),
        case.name.clone(),
        s("-o"),
        job.to_string_lossy().into_owned(),
    ];
    let pack = run_cmd(&argv, workdir, timeout, &[]);
    if pack.rc != 0 || !job.exists() {
        case.s_bucket = Some(s(crate::axes::solver::UNSUPPORTED));
        let why = tail(&pack.err);
        case.s_note = if why.is_empty() { format!("could not package the plan (exit {})", pack.rc) } else { why };
    }
    pack.wall
}

/// The second stage: prove equivalence of the plan `json_name` in `workdir`, within `timeout`
/// seconds, and within `mem_bytes` of address space when that is set.
pub fn prove(
    case: &mut Case,
    workdir: &Path,
    json_name: &str,
    prover: &str,
    timeout: f64,
    smt_timeout_ms: Option<u64>,
    mem_bytes: Option<u64>,
) {
    let env: Vec<(String, String)> =
        smt_timeout_ms.map(|ms| vec![(s("QED_SMT_TIMEOUT"), ms.to_string())]).unwrap_or_default();
    let argv = [prover.to_string(), json_name.to_string()];
    let qr = crate::proc::run_limited(&argv, Some(workdir), &env, Some(timeout), mem_bytes);
    case.prove_wall = qr.wall;
    if qr.timed_out {
        case.status = s(TIMEOUT);
        case.message = s("prover timed out");
        return;
    }
    let stem = json_name.strip_suffix(".json").unwrap_or(json_name);
    read_result(case, &workdir.join(format!("{stem}.result")), &qr);
}

/// The last 600 characters of the prover's stdout and of its stderr: what tells one crash from
/// another (docs/INTERNALS.md has the taxonomy).
fn crash_tail(qr: &crate::proc::Run) -> String {
    let last = |t: &str| {
        let n = t.chars().count();
        t.chars().skip(n.saturating_sub(600)).collect::<String>()
    };
    format!("{}\n--- stderr ---\n{}", last(qr.out.trim_end()), last(qr.err.trim_end()))
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
            (case.q_tail, case.q_rc) = (crash_tail(qr), Some(qr.rc));
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
        (case.q_tail, case.q_rc) = (crash_tail(qr), Some(qr.rc));
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
