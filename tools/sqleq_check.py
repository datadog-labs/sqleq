#!/usr/bin/env python3
"""sqleq-check — a batch harness that runs SQL equivalence pairs past the QED prover.

Takes `.sql` files (or directories of them), runs each through the Rust
frontend (SQL -> JSON relational plan) and then the QED prover (JSON ->
equivalence verdict), and produces a consolidated, CI-friendly report.

Each input `.sql` file must declare its table schemas / functions and
contain **exactly two** `SELECT` queries; the harness reports whether the
two are provably equivalent under bag semantics.

This used to shell out to the Java/Calcite `qed-parser` for the first stage.
It no longer does, and there is no JVM anywhere in the pipeline: the frontend
is a single Rust binary built from this repo. The stage it replaced is the
reason the taxonomy below says `refused` rather than `parse_error` — the
frontend declines constructs it cannot lower *faithfully*, which is a
deliberate soundness choice and not the same event as a parse failure, so the
two are distinguished by `refuse_kind` on each case.

Every report is split into **trivial** and **non-trivial** pairs. A pair is
trivial when its two queries reach the prover identical — the preprocessor's
own (equivalence-preserving) normalizations collapsed the difference, so the
prover is confirming `x = x`. Proving those is sound but not evidence of
capability, and in practice they are often the large majority, so the summary
prints `capability` — proved among the pairs that actually differ — directly
beneath the raw `proved` count.

`--sqlsolver` adds a **second opinion** on the same cases: SQLSolver, run over
the very `Input` JSON this harness hands the QED prover, through the bridge in
`tools/sqlsolver/`. It is off by default and it never changes the exit code —
the qed axis decides the policy — because it answers a different question. Two
things
must be read off it carefully, and the summary says both:

* **That prover never disproves.** Its `NEQ` means "no proof found", exactly
  like its `UNKNOWN`; only its `EQ` is a claim. A case we prove and it calls
  `NEQ` is not a contradiction, and nothing here may treat it as one. `sqleq-fuzz`
  is the only disprover in this project.
* **The two opinions share a frontend, so they are not independent.** A lowering
  bug yields the same wrong plan on both axes; agreement corroborates the
  provers, not the frontend.

The harness itself is dependency-free (Python 3.8+ standard library only).
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field, asdict
from glob import glob
from pathlib import Path
from typing import Optional

# ---------------------------------------------------------------------------
# Binary discovery
# ---------------------------------------------------------------------------

REPO = Path(__file__).resolve().parents[1]


def _newest(paths: list[str]) -> Optional[str]:
    paths = [p for p in paths if os.path.isfile(p) and os.access(p, os.X_OK)]
    if not paths:
        return None
    return max(paths, key=lambda p: os.stat(p).st_mtime)


def discover_frontend(override: Optional[str]) -> str:
    """Resolve sqleq-frontend: explicit override -> $SQLEQ_FRONTEND -> PATH -> this
    repo's own build (release preferred over debug)."""
    for c in (override, os.environ.get("SQLEQ_FRONTEND")):
        if c:
            if os.path.isfile(c) and os.access(c, os.X_OK):
                return c
            sys.exit(f"error: sqleq-frontend not found or not executable at: {c}")
    found = shutil.which("sqleq-frontend")
    if found:
        return found
    local = _newest([str(REPO / "target" / p / "sqleq-frontend")
                     for p in ("release", "debug")])
    if local:
        return local
    sys.exit(
        "error: could not find 'sqleq-frontend'. Build it with "
        "`cargo build --release`, put it on PATH, or pass --frontend/$SQLEQ_FRONTEND."
    )


def discover_prover(override: Optional[str]) -> str:
    """Resolve qed-prover: explicit override -> $QED_PROVER -> PATH -> /nix/store."""
    for c in (override, os.environ.get("QED_PROVER")):
        if c:
            if os.path.isfile(c) and os.access(c, os.X_OK):
                return c
            sys.exit(f"error: qed-prover not found or not executable at: {c}")
    found = shutil.which("qed-prover")
    if found:
        return found
    # Fallback: the Nix-wrapped prover (it carries z3 + cvc5 on its own PATH),
    # useful when not inside the dev shell.
    store = _newest(glob("/nix/store/*-qed-prover*/bin/qed-prover"))
    if store:
        return store
    sys.exit(
        "error: could not find 'qed-prover'. Enter the QED Nix shell, or pass "
        "--prover/$QED_PROVER."
    )


# ---------------------------------------------------------------------------
# Subprocess helper with a hard wall-clock timeout (kills the process group so
# the prover's z3/cvc5 children don't get orphaned).
# ---------------------------------------------------------------------------


@dataclass
class Run:
    rc: int
    out: str
    err: str
    timed_out: bool
    wall: float


def run_cmd(cmd: list[str], cwd: str, timeout: float, env: Optional[dict] = None) -> Run:
    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,  # own process group for clean timeout kills
    )
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            out, err = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            out, err = "", ""
    return Run(proc.returncode or 0, out or "", err or "", timed_out, time.monotonic() - t0)


# ---------------------------------------------------------------------------
# Triviality
# ---------------------------------------------------------------------------
#
# Many pairs reach the prover as `x` against `x`: the preprocessor's
# normalizations are themselves equivalence-preserving rewrites, and on some
# pairs the rewrite they undo *is* the optimization under test. The end-to-end
# claim stays sound — normalize soundly, then prove — but the credit does not
# belong to the prover, so a raw "proved N/M" can overstate capability badly.
# Every report therefore carries the split.


def _statements(sql: str) -> list[str]:
    """Split on top-level `;`, dropping declarations. Quote- and comment-aware,
    because a `;` inside a string literal or a `--` comment does not end a
    statement and mis-splitting would silently mis-classify the case."""
    out, buf, quote, i = [], [], "", 0
    while i < len(sql):
        ch = sql[i]
        if quote:
            buf.append(ch)
            if ch == quote:
                # A doubled quote is an escaped quote, not a terminator.
                if i + 1 < len(sql) and sql[i + 1] == quote:
                    buf.append(sql[i + 1])
                    i += 2
                    continue
                quote = ""
            i += 1
            continue
        if ch in "'\"":
            quote = ch
            buf.append(ch)
        elif sql.startswith("--", i):
            nl = sql.find("\n", i)
            i = len(sql) if nl < 0 else nl
            continue
        elif sql.startswith("/*", i):
            end = sql.find("*/", i + 2)
            i = len(sql) if end < 0 else end + 2
            continue
        elif ch == ";":
            out.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
        i += 1
    out.append("".join(buf))

    keep = []
    for s in out:
        s = " ".join(s.split())
        if not s:
            continue
        head = s.lower()
        if head.startswith(("create ", "declare ", "drop ", "set ", "insert ")):
            continue
        keep.append(s)
    return keep


def triviality_from_text(sql: str) -> Optional[bool]:
    """True when the two queries are textually identical after whitespace
    normalization. Weaker than the IR test — it misses alias-only and quoting
    differences — and used only when there is no IR to look at."""
    qs = _statements(sql)
    if len(qs) != 2:
        return None
    return qs[0] == qs[1]


def triviality_from_ir(plan: dict) -> Optional[bool]:
    """True when the two lowered query plans are structurally equal, i.e. the
    prover was handed `x` against `x` and equivalence is reflexivity. This is
    the definition that matters: it is exactly what the prover sees, so it also
    catches pairs that differ only in aliases, quoting or whitespace."""
    qs = plan.get("queries")
    if not isinstance(qs, list) or len(qs) != 2:
        return None
    return qs[0] == qs[1]


# ---------------------------------------------------------------------------
# Per-case execution
# ---------------------------------------------------------------------------

# Status taxonomy
PROVABLE = "provable"        # prover proved the two queries equivalent
UNPROVABLE = "unprovable"    # prover ran but could not prove equivalence
REFUSED = "refused"          # frontend would not lower the SQL (see refuse_kind)
PANIC = "panic"              # prover panicked / crashed on the case
TIMEOUT = "timeout"          # exceeded the per-case wall-clock budget
ERROR = "error"              # anything else (e.g. JSON it couldn't read)

STATUS_ORDER = [PROVABLE, UNPROVABLE, TIMEOUT, REFUSED, PANIC, ERROR]

# The second opinion has its own vocabulary, deliberately disjoint from the one
# above so the two can never be averaged into a single "status". The collapse of
# `NEQ` and `UNKNOWN` into one bucket is the whole point: that prover does not
# disprove, so keeping its `NEQ` under its own name would invite someone to read
# it as a counterexample. `proved-literal` is its tier 0 — the two plans were
# already identical — which is the same fact this harness calls `trivial`, and it
# is held apart from `proved` for the same reason.
SQLSOLVER_PROVED = "proved"
SQLSOLVER_PROVED_LITERAL = "proved-literal"
SQLSOLVER_NO_PROOF = "no-proof"
SQLSOLVER_UNSUPPORTED = "unsupported"   # our bridge could not build a plan from the IR
SQLSOLVER_TIMEOUT = "timeout"
SQLSOLVER_ERROR = "error"
SQLSOLVER_MISSING = "missing"           # never answered: JVM died, or no job was built
SQLSOLVER_ORDER = [SQLSOLVER_PROVED, SQLSOLVER_PROVED_LITERAL, SQLSOLVER_NO_PROOF,
                   SQLSOLVER_UNSUPPORTED, SQLSOLVER_TIMEOUT, SQLSOLVER_ERROR,
                   SQLSOLVER_MISSING]

# `IrDriver`'s raw verdicts. `NOTRANS` and `NOIR` are ours, not theirs — the
# bridge never handed their prover a plan — and they stay out of `no-proof`
# for that reason: conflating "we could not express it" with "they could not
# prove it" is what made their parser look like their prover historically.
_SQLSOLVER_BUCKET = {
    "EQ": SQLSOLVER_PROVED, "NEQ": SQLSOLVER_NO_PROOF,
    "UNKNOWN": SQLSOLVER_NO_PROOF,
    "NOTRANS": SQLSOLVER_UNSUPPORTED, "NOIR": SQLSOLVER_UNSUPPORTED,
    "TIMEOUT": SQLSOLVER_TIMEOUT, "HANG": SQLSOLVER_TIMEOUT,
    "ERROR": SQLSOLVER_ERROR,
}


@dataclass
class Case:
    name: str            # display name (path relative to a common root)
    path: str            # absolute source .sql path
    status: str = ERROR
    wall: float = 0.0            # harness-measured total wall time (s)
    lower_wall: float = 0.0
    prove_wall: float = 0.0
    refuse_kind: str = ""        # parse | unsupported | schema, when refused
    complete_fragment: bool = False
    smt_timed_out: bool = False
    nontrivial_perms: bool = False
    trivial: Optional[bool] = None   # the two queries are the same; None = undetermined
    trivial_basis: str = ""          # "ir" | "text" — how `trivial` was decided
    stats: dict = field(default_factory=dict)  # full prover Stats from .result
    message: str = ""    # error detail when not provable/unprovable
    # The second opinion, only when --sqlsolver is on. `s_verdict` is SQLSolver's
    # raw label, kept beside the bucket purely for audit: nothing may branch on
    # it, because `NEQ` is not a refutation.
    s_bucket: Optional[str] = None
    s_verdict: Optional[str] = None
    s_ms: Optional[int] = None
    s_note: str = ""


def classify_refusal(err: str) -> tuple[str, str]:
    """Map the frontend's stderr to (refuse_kind, one-line reason).

    The three kinds mirror FrontendError: a `PARSE ERROR:` prefix means sqlparser
    rejected the text, `unsupported:` means we declined to lower a construct, and
    anything else is a schema/shape complaint (unresolved column, wrong number of
    queries, bad DDL) — which Display leaves unprefixed because those messages are
    already self-describing.
    """
    lines = [ln for ln in err.splitlines() if ln.strip()]
    reason = lines[-1].strip() if lines else "frontend produced no JSON output"
    if reason.startswith("PARSE ERROR"):
        return "parse", reason
    if reason.startswith("unsupported:"):
        return "unsupported", reason
    return "schema", reason


def set_triviality(case: Case, plan_path: Optional[Path], src: Path) -> None:
    """Record whether this case is `x` against `x`. Prefers the IR test and
    falls back to the source text when there is no IR — a refusal still has a
    triviality, and it is worth knowing whether the refusals are landing on the
    pairs that would have counted."""
    if plan_path is not None:
        try:
            verdict = triviality_from_ir(json.loads(plan_path.read_text()))
        except (OSError, ValueError):
            verdict = None
        if verdict is not None:
            case.trivial, case.trivial_basis = verdict, "ir"
            return
    if src.suffix != ".json":
        try:
            verdict = triviality_from_text(src.read_text())
        except OSError:
            verdict = None
        if verdict is not None:
            case.trivial, case.trivial_basis = verdict, "text"


def run_case(
    src: Path,
    name: str,
    frontend: str,
    prover: str,
    case_timeout: float,
    smt_timeout_ms: Optional[int],
    keep_dir: Optional[Path],
    ss_dir: Optional[Path] = None,
) -> Case:
    case = Case(name=name, path=str(src))
    t0 = time.monotonic()

    if keep_dir is not None:
        work = keep_dir / name.replace("/", "__")
        work.mkdir(parents=True, exist_ok=True)
        workdir = str(work)
        cleanup = False
    else:
        workdir = tempfile.mkdtemp(prefix="sqleq-")
        cleanup = True

    try:
        stem = src.stem
        json_path = Path(workdir) / f"{stem}.json"

        if src.suffix == ".json":
            # Pre-parsed relational plan: skip the frontend stage entirely.
            shutil.copyfile(src, json_path)
        else:
            # 1) Lower SQL -> JSON
            local_sql = Path(workdir) / f"{stem}.sql"
            shutil.copyfile(src, local_sql)
            fr = run_cmd([frontend, local_sql.name, json_path.name], workdir, case_timeout)
            case.lower_wall = fr.wall
            if fr.timed_out:
                case.status = TIMEOUT
                case.message = "frontend timed out"
                set_triviality(case, None, src)
                case.wall = time.monotonic() - t0
                return case
            # The frontend's exit code is meaningful (unlike the Java parser's),
            # but check the artifact too: a zero exit with no JSON is still a
            # case we cannot prove, and silently proving nothing would be worse.
            if fr.rc != 0 or not json_path.exists() or json_path.stat().st_size == 0:
                case.status = REFUSED
                case.refuse_kind, case.message = classify_refusal(fr.err)
                set_triviality(case, None, src)
                case.wall = time.monotonic() - t0
                return case

        set_triviality(case, json_path, src)

        # 1b) Package the plan for the second opinion, while the workdir still
        # exists. This is `{name, ir, schema}` built from *this* JSON — the same
        # bytes the prover is about to read — so nothing re-lowers the case and
        # the two axes cannot drift apart between here and `IrDriver`. Built
        # before the prover runs, so a prover timeout does not also cost us the
        # second opinion; its own cost is discounted from `case.wall` below so a
        # `--sqlsolver` run's timings stay comparable to one without it.
        if ss_dir is not None:
            job = ss_dir / f"{ss_slug(name)}.job.jsonl"
            pack = run_cmd([frontend, "--sqlsolver", "--ir", json_path.name,
                            "--name", name, "-o", str(job)], workdir, case_timeout)
            t0 += pack.wall
            if pack.rc != 0 or not job.exists():
                case.s_bucket = SQLSOLVER_UNSUPPORTED
                case.s_note = _tail(pack.err) or f"could not package the plan (exit {pack.rc})"

        # 2) Prove equivalence
        env = dict(os.environ)
        if smt_timeout_ms is not None:
            env["QED_SMT_TIMEOUT"] = str(smt_timeout_ms)
        remaining = max(1.0, case_timeout - case.lower_wall)
        qr = run_cmd([prover, json_path.name], workdir, remaining, env)
        case.prove_wall = qr.wall

        if qr.timed_out:
            case.status = TIMEOUT
            case.message = "prover timed out"
            case.wall = time.monotonic() - t0
            return case

        result_path = Path(workdir) / f"{stem}.result"
        stats = {}
        if result_path.exists():
            try:
                stats = json.loads(result_path.read_text())
            except json.JSONDecodeError:
                stats = {}
        case.stats = stats

        if stats:
            case.complete_fragment = bool(stats.get("complete_fragment"))
            case.smt_timed_out = bool(stats.get("smt_timed_out"))
            case.nontrivial_perms = bool(stats.get("nontrivial_perms"))
            if stats.get("panicked"):
                case.status = PANIC
                case.message = "prover panicked"
            elif stats.get("provable"):
                case.status = PROVABLE
            else:
                case.status = UNPROVABLE
        else:
            # No .result written: infer from stdout, else treat as panic/error.
            if "is provable for" in qr.out and "is not provable for" not in qr.out:
                case.status = PROVABLE
            elif "is not provable for" in qr.out:
                case.status = UNPROVABLE
            elif qr.rc != 0:
                case.status = PANIC
                case.message = (qr.err or qr.out).strip()[:400] or f"prover exit {qr.rc}"
            else:
                case.status = ERROR
                case.message = "no .result and no verdict on stdout"

        case.wall = time.monotonic() - t0
        return case
    finally:
        if cleanup:
            shutil.rmtree(workdir, ignore_errors=True)


# ---------------------------------------------------------------------------
# The sqlsolver axis — the second opinion
# ---------------------------------------------------------------------------
#
# SQLSolver is a second equivalence prover, an independent implementation rather
# than a variant of this one. It used to be reachable only through its own
# MySQL-dialect parser, which is where most of its answers were lost; the bridge
# in `tools/sqlsolver/` hands it our lowered `Input` instead, so the question it
# is asked here is literally the one the QED prover is asked. `docs/SQLSOLVER.md`
# has the measurements.
#
# Three properties of that prover shape the code below, and together they are why
# this is one batched pass at the end rather than a call inside `run_case`:
#
#   * it is a JVM, so per-case startup would swamp the cases themselves;
#   * `Verification.verify` can hang in a way interrupts do not reach, so the
#     driver halts its own process after writing the offending row and expects
#     the harness to resume on a fresh one — the loop in `run_second_opinion`;
#   * its per-row cap is load-sensitive, so the rows go through sequentially even
#     when the prover pass ran them `-j` wide. A second opinion that changes
#     under load is not one.

# There is deliberately no default location for either. The fork and its exploded
# dependency directory live wherever their operator built them, and a default that
# resolves on one machine only fails as "no classes there" rather than as "you did
# not say where" -- which is the more expensive error to debug.


def _tail(text: str) -> str:
    """The last non-empty line — for a one-line reason, which is the useful part
    of a multi-line Java stack or a frontend refusal."""
    lines = [ln.strip() for ln in (text or "").splitlines() if ln.strip()]
    return lines[-1] if lines else ""


def ss_slug(name: str) -> str:
    """A case's display name may be a relative path; a job file's name may not."""
    return re.sub(r"[^A-Za-z0-9._-]+", "_", name) or "case"


def _compile_driver(cp: str) -> Path:
    """Rebuild the bridge driver when its sources are newer than its class, so a
    stale translator can never be paired with fresh jobs.

    Staleness is keyed on the newest of `IrDriver.java` and `IrToRel.java` rather
    than on the entry point alone: `IrToRel` is the file that imports one Calcite
    or the other, so it is the one that actually differs between the pristine tree
    and the de-Calcited fork — which is also why the output directory is
    `out-fork/` and is not shared with `out/`.
    """
    srcs = [REPO / "tools" / "sqlsolver" / f"{n}.java" for n in ("IrDriver", "IrToRel")]
    missing = [str(f) for f in srcs if not f.is_file()]
    if missing:
        sys.exit("error: the bridge sources are missing: " + ", ".join(missing))
    out = REPO / "tools" / "sqlsolver" / "out-fork"
    cls = out / "IrDriver.class"
    if cls.is_file() and cls.stat().st_mtime >= max(f.stat().st_mtime for f in srcs):
        return out
    out.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(["javac", "--release", "17", "-proc:none", "-cp", cp,
                        "-d", str(out)] + [str(f) for f in srcs],
                       capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit("error: cannot compile the bridge driver:\n" + (r.stderr or "").strip())
    return out


def discover_sqlsolver(override: Optional[str]) -> tuple[str, Path]:
    """Resolve the second prover as (classpath, working directory).

    Override -> $SQLEQ_SQLSOLVER, and one of the two must say where. There is no
    PATH lookup and no jar to run: Gradle 7.4 cannot run on a JDK 21, so the
    fork is compiled with javac into `build/classes-javac` and driven against
    $SQLEQ_SQLSOLVER_DEPS — the pristine fat jar exploded with
    `org/apache/calcite` removed. Keeping that directory on the classpath
    instead of the original jar is also what keeps the Calcite removal honest:
    a surviving reference could not resolve.

    The working directory is not incidental. `sqlsolver.properties` and
    `sqlsolver_data/` are read relative to the tree root, and the Z3 bindings are
    loaded from `lib/` — which needs both `-Djava.library.path` and
    `LD_LIBRARY_PATH`, one for the JVM's own lookup and one for the dependent
    `.so` the first one pulls in.
    """
    root = override or os.environ.get("SQLEQ_SQLSOLVER")
    if not root:
        sys.exit("error: --sqlsolver needs the fork's location: pass "
                 "--sqlsolver-tree DIR or set $SQLEQ_SQLSOLVER.")
    tree = Path(root)
    classes = tree / "build" / "classes-javac"
    if not classes.is_dir():
        sys.exit(f"error: no SQLSolver fork classes at {classes}.\n"
                 f"       Build the fork, or point --sqlsolver-tree / "
                 f"$SQLEQ_SQLSOLVER at a tree that has them.")
    dep_root = os.environ.get("SQLEQ_SQLSOLVER_DEPS")
    if not dep_root:
        sys.exit("error: set $SQLEQ_SQLSOLVER_DEPS to the exploded fat jar; the "
                 "fork's classpath is not derivable from the tree.")
    deps = Path(dep_root)
    if not deps.is_dir():
        sys.exit(f"error: no dependency directory at {deps}; "
                 f"set $SQLEQ_SQLSOLVER_DEPS to the exploded jar.")
    if not shutil.which("javac") or not shutil.which("java"):
        sys.exit("error: --sqlsolver needs a JDK on PATH (javac and java).")
    driver = _compile_driver(f"{deps}:{classes}")
    return f"{deps}:{classes}:{driver}", tree


def _ss_answered(path: Path) -> dict:
    """The rows the driver has already written, by name.

    A self-halt can truncate the final line mid-write, so an unparseable tail is
    dropped rather than trusted: that row is simply re-run on the next pass."""
    out: dict = {}
    if not path.exists():
        return out
    with path.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if "name" in rec:
                out[rec["name"]] = rec
    return out


def run_second_opinion(cases: list[Case], ss_dir: Path, cp: str, cwd: Path,
                       timeout_ms: int) -> dict:
    """Ask SQLSolver about every case that produced a job, and attach the answers.

    Mutates the cases in place, because the answer belongs beside the case rather
    than in a parallel table that a later sort could desynchronise.
    """
    jobs = []
    for case in cases:
        job = ss_dir / f"{ss_slug(case.name)}.job.jsonl"
        if not job.exists():
            # Refused, or lowered but not packageable. Either way the bridge never
            # got a plan, and that is ours, not theirs — hence `unsupported`.
            if case.s_bucket is None:
                case.s_bucket = SQLSOLVER_UNSUPPORTED
                # Prefer the frontend's own reason: on a refused row it names the
                # construct, which is what a reader of this column wants. Falls
                # back only when there is genuinely nothing to say.
                case.s_note = case.s_note or _tail(case.message) or "no plan to bridge"
            continue
        try:
            jobs.append(json.loads(job.read_text().splitlines()[0]))
        except (OSError, ValueError, IndexError) as e:
            case.s_bucket, case.s_note = SQLSOLVER_ERROR, f"unreadable job: {e}"
    if not jobs:
        return {"rows": 0, "passes": 0, "halts": 0}

    todo_path = ss_dir / "todo.jsonl"
    out_path = ss_dir / "results.jsonl"
    env = dict(os.environ, LD_LIBRARY_PATH=str(cwd / "lib"))
    cmd_head = ["java", f"-Djava.library.path={cwd / 'lib'}", "-cp", cp, "IrDriver"]
    passes, halts, stall = 0, 0, None
    while True:
        have = _ss_answered(out_path)
        todo = [j for j in jobs if j["name"] not in have]
        if not todo:
            break
        todo_path.write_text("".join(json.dumps(j) + "\n" for j in todo))
        passes += 1
        proc = subprocess.run(
            cmd_head + [str(todo_path), str(out_path), f"--timeout-ms={timeout_ms}"],
            cwd=str(cwd), env=env, capture_output=True, text=True)
        # Exit 3 is the driver taking its own JVM down because a row ignored its
        # interrupt. It writes the row first, so resuming always advances.
        if proc.returncode == 3:
            halts += 1
            continue
        if len(_ss_answered(out_path)) <= len(have):
            # No row was answered and the process is gone: a classpath or native
            # library problem, not a hard case. Report it rather than spinning.
            stall = {"exit": proc.returncode, "reason": _tail(proc.stderr)}
            break

    answered = _ss_answered(out_path)
    for case in cases:
        rec = answered.get(case.name)
        if rec is None:
            if case.s_bucket is None:
                case.s_bucket = SQLSOLVER_MISSING
                case.s_note = (stall or {}).get("reason", "") or "no answer"
            continue
        case.s_verdict = rec.get("verdict")
        case.s_ms = rec.get("ms")
        bucket = _SQLSOLVER_BUCKET.get(case.s_verdict or "", SQLSOLVER_ERROR)
        # Their tier 0: the two plans were already identical, so no proving
        # happened. Held apart from a real proof for the same reason this harness
        # holds `trivial` apart from `capability`.
        if bucket == SQLSOLVER_PROVED and rec.get("literal"):
            bucket = SQLSOLVER_PROVED_LITERAL
        # Their prover answers UNKNOWN when interrupted, so a row killed at the cap
        # arrives indistinguishable from one it considered and declined. `killed` is
        # the only thing that separates them, and the distinction is the whole point
        # of a `timeout` bucket: "we stopped asking" is not "they found no proof".
        # An EQ is exempt -- a proof that landed on the boundary is still a claim.
        if rec.get("killed") and bucket not in (SQLSOLVER_PROVED, SQLSOLVER_PROVED_LITERAL):
            bucket = SQLSOLVER_TIMEOUT
        case.s_bucket = bucket
        case.s_note = _tail(rec.get("refused") or rec.get("error") or "")

    out = {"rows": len(jobs), "passes": passes, "halts": halts,
           "answered": len(answered)}
    if stall:
        out["stalled"] = stall
    return out


# ---------------------------------------------------------------------------
# Input collection
# ---------------------------------------------------------------------------


INPUT_EXTS = (".sql", ".json")


def collect_inputs(paths: list[str]) -> list[Path]:
    """Collect .sql and .json inputs. When a .sql and a .json share the same
    directory+stem (i.e. the .json was generated from that .sql), keep only the
    .sql so we don't run the same case twice."""
    found: list[Path] = []
    seen = set()
    for p in paths:
        pp = Path(p)
        if pp.is_dir():
            for ext in INPUT_EXTS:
                for f in pp.rglob(f"*{ext}"):
                    rp = f.resolve()
                    if rp not in seen:
                        seen.add(rp)
                        found.append(f)
        elif pp.is_file() and pp.suffix in INPUT_EXTS:
            rp = pp.resolve()
            if rp not in seen:
                seen.add(rp)
                found.append(pp)
        else:
            print(f"warning: skipping unsupported input: {p}", file=sys.stderr)

    sql_stems = {(f.resolve().parent, f.stem) for f in found if f.suffix == ".sql"}
    deduped = [
        f for f in found
        if not (f.suffix == ".json" and (f.resolve().parent, f.stem) in sql_stems)
    ]
    return sorted(deduped, key=lambda f: str(f).lower())


def common_root(files: list[Path]) -> Path:
    if not files:
        return Path(".")
    try:
        return Path(os.path.commonpath([str(f.resolve()) for f in files]))
    except ValueError:
        return Path(".")


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


class Color:
    def __init__(self, on: bool):
        self.on = on

    def _w(self, code: str, s: str) -> str:
        return f"\033[{code}m{s}\033[0m" if self.on else s

    def green(self, s): return self._w("32", s)
    def red(self, s): return self._w("31", s)
    def yellow(self, s): return self._w("33", s)
    def dim(self, s): return self._w("2", s)
    def bold(self, s): return self._w("1", s)


STATUS_GLYPH = {
    PROVABLE: ("✓", "green"),
    UNPROVABLE: ("✗", "yellow"),
    TIMEOUT: ("⏱", "yellow"),
    REFUSED: ("⚠", "yellow"),
    PANIC: ("💥", "red"),
    ERROR: ("?", "red"),
}


def fmt_status(c: Color, status: str) -> str:
    glyph, col = STATUS_GLYPH.get(status, ("?", "red"))
    return getattr(c, col)(f"{glyph} {status}")


def print_case_line(c: Color, case: Case, name_w: int):
    extra = []
    if case.complete_fragment:
        extra.append("complete-frag")
    if case.smt_timed_out:
        extra.append(c.yellow("smt-timeout"))
    if case.nontrivial_perms:
        extra.append("perm")
    tag = ("  " + c.dim(" ".join(extra))) if extra else ""
    # Show the last non-empty line — for a refusal that is the frontend's own
    # one-line reason, which is the useful part.
    msg = ""
    if case.message:
        tail = [ln for ln in case.message.splitlines() if ln.strip()]
        if tail:
            msg = "  " + c.dim("— " + tail[-1].strip())
    print(
        f"  {fmt_status(c, case.status):<22} {case.name:<{name_w}}  "
        f"{c.dim(f'{case.wall:6.2f}s')}{tag}{msg}"
    )


def triviality_split(cases: list[Case]) -> dict:
    """Counts by (status, trivial). `capability` is the headline: how many pairs
    that actually differ were proved."""
    nontrivial = [x for x in cases if x.trivial is False]
    trivial = [x for x in cases if x.trivial is True]
    return {
        "trivial": len(trivial),
        "nontrivial": len(nontrivial),
        "undetermined": sum(1 for x in cases if x.trivial is None),
        "by_text": sum(1 for x in cases if x.trivial_basis == "text"),
        "trivial_proved": sum(1 for x in trivial if x.status == PROVABLE),
        "capability": sum(1 for x in nontrivial if x.status == PROVABLE),
        "capability_of": len(nontrivial),
    }


def print_capability(c: Color, cases: list[Case]):
    """The proved/total line above counts reflexive pairs, which the prover did
    not earn. Print what it did earn, right underneath, so the two numbers are
    never seen apart."""
    s = triviality_split(cases)
    if not s["capability_of"] and not s["trivial"]:
        return
    if s["capability_of"]:
        pct = 100.0 * s["capability"] / s["capability_of"]
        print(f"  {c.bold('capability'):<13} {s['capability']}/{s['capability_of']}  "
              f"({pct:.1f}%)   {c.dim('pairs whose two queries differ')}")
    else:
        print(f"  {c.bold('capability'):<13} n/a"
              f"           {c.dim('no pair here has two differing queries')}")
    detail = (f"{s['trivial_proved']}/{s['trivial']} of the reflexive "
              f"(x vs x) pairs also proved")
    if s["undetermined"]:
        detail += f"; {s['undetermined']} undetermined"
    print(c.dim(f"  {'':<13} {detail}"))


def print_summary(c: Color, cases: list[Case], wall: float):
    counts = {s: 0 for s in STATUS_ORDER}
    for case in cases:
        counts[case.status] = counts.get(case.status, 0) + 1
    total = len(cases)
    print()
    print(c.bold("  Summary"))
    print(c.dim("  " + "─" * 40))
    for s in STATUS_ORDER:
        if counts[s]:
            label = fmt_status(c, s)
            print(f"  {label:<22} {counts[s]:>5}")
            # Refusals are a soundness feature, not a bug — break them down
            # under their own row so deliberate declines are distinguishable
            # from parse gaps at a glance.
            if s == REFUSED:
                kinds = {}
                for case in cases:
                    if case.status == REFUSED:
                        kinds[case.refuse_kind] = kinds.get(case.refuse_kind, 0) + 1
                detail = ", ".join(f"{k} {n}" for k, n in sorted(kinds.items()))
                print(c.dim(f"  {'':<24}{detail}"))
    print(c.dim("  " + "─" * 40))
    provable = counts[PROVABLE]
    pct = (100.0 * provable / total) if total else 0.0
    print(f"  {c.bold('proved'):<13} {provable}/{total}  ({pct:.1f}%)")
    print_capability(c, cases)
    print(f"  {c.dim('wall time'):<13} {wall:.2f}s")
    if cases:
        avg = sum(x.wall for x in cases) / len(cases)
        slowest = max(cases, key=lambda x: x.wall)
        print(f"  {c.dim('avg / case'):<13} {avg:.2f}s   "
              f"{c.dim('slowest')} {slowest.name} ({slowest.wall:.2f}s)")


def print_second_opinion(c: Color, cases: list[Case], stats: dict):
    """The second opinion, and the two warnings that have to travel with it."""
    scored = [x for x in cases if x.s_bucket is not None]
    if not scored:
        return
    counts: dict = {}
    for x in scored:
        counts[x.s_bucket] = counts.get(x.s_bucket, 0) + 1
    print()
    print(c.bold("  Second opinion") + c.dim("  — SQLSolver, over the same lowered IR"))
    print(c.dim("  " + "─" * 40))
    for b in SQLSOLVER_ORDER:
        if counts.get(b):
            print(f"  {b:<22} {counts[b]:>5}")
    print(c.dim("  " + "─" * 40))

    # The cells below stand on the same footing as `capability` above: pairs whose
    # two queries actually differ, and real proofs only. A pair that reaches
    # either prover as `x` against `x` was answered by neither — their
    # `proved-literal` is this harness's `trivial` seen from the other side — so
    # counting it would inflate both columns and the agreement between them.
    diff = [x for x in scored if x.trivial is False]
    ours = {x.name for x in diff if x.status == PROVABLE}
    theirs = {x.name for x in diff if x.s_bucket == SQLSOLVER_PROVED}
    both, only_p, only_s = ours & theirs, ours - theirs, theirs - ours
    print(f"  {'of pairs that differ':<22} {len(diff):>5}")
    print(f"  {'both provers':<22} {len(both):>5}")
    print(f"  {'only the QED prover':<22} {len(only_p):>5}")
    print(f"  {'only SQLSolver':<22} {c.bold(f'{len(only_s):>5}')}   "
          f"{c.dim('what the second opinion adds')}")
    for name in sorted(only_s)[:10]:
        print(c.dim(f"  {'':<22}       {name}"))
    if len(only_s) > 10:
        print(c.dim(f"  {'':<22}       … and {len(only_s) - 10} more"))
    print(f"  {'neither':<22} {len(diff) - len(both | only_p | only_s):>5}")
    if stats.get("wall_s") is not None:
        detail = f"{stats['wall_s']:.2f}s over {stats.get('answered', 0)} row(s)"
        if stats.get("halts"):
            detail += (f", {stats['halts']} JVM self-halt(s) in "
                       f"{stats.get('passes', 0)} pass(es)")
        print(c.dim(f"  {'wall time':<22} {detail}"))
    if stats.get("stalled"):
        st = stats["stalled"]
        print(c.red(f"  stalled: IrDriver exited {st['exit']} without answering a "
                    f"row — {st['reason'] or 'no message'}"))
    print(c.dim("  note  `no-proof` is not a refutation. That prover's NEQ means "
                "\"no proof\n        found\", exactly like its UNKNOWN; only its EQ "
                "is a claim, and\n        sqleq-fuzz remains the only disprover here."))
    print(c.dim("        Both opinions come through this repo's frontend, so where "
                "they\n        agree they corroborate the provers, not the lowering."))


def write_json(path: str, cases: list[Case], meta: dict):
    payload = {"meta": meta, "cases": [asdict(x) for x in cases]}
    Path(path).write_text(json.dumps(payload, indent=2))


def write_csv(path: str, cases: list[Case]):
    cols = ["name", "status", "trivial", "trivial_basis", "refuse_kind", "wall",
            "lower_wall", "prove_wall", "complete_fragment", "smt_timed_out",
            "nontrivial_perms", "message",
            "s_bucket", "s_verdict", "s_ms", "s_note"]
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(cols)
        for c in cases:
            w.writerow([getattr(c, k) for k in cols])


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="sqleq-check",
        description="Batch SQL equivalence checking with the QED prover.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Each .sql file must contain table/function declarations and "
            "exactly two SELECT queries to compare.\n\n"
            "Exit codes:\n"
            "  0  policy satisfied (see --expect)\n"
            "  1  policy not satisfied (some case failed expectation)\n"
            "  2  usage / setup error\n"
        ),
    )
    p.add_argument("paths", nargs="+", metavar="PATH",
                   help="One or more .sql / .json files or directories (recursed). "
                        ".json inputs are treated as pre-parsed plans and skip the "
                        "frontend stage. A .sql and its sibling .json are de-duplicated.")
    p.add_argument("-j", "--jobs", type=int, default=min(8, os.cpu_count() or 4),
                   help="Parallel cases (default: min(8, ncpu)). Each case itself "
                        "runs z3+cvc5, so avoid heavy oversubscription.")
    p.add_argument("-t", "--timeout", type=float, default=60.0,
                   help="Per-case wall-clock timeout in seconds (default: 60).")
    p.add_argument("--smt-timeout", type=int, default=None, metavar="MS",
                   help="QED_SMT_TIMEOUT for each SMT request, in ms "
                        "(default: prover's own default of 10000).")
    p.add_argument("--expect", choices=["equivalent", "report-only"],
                   default="equivalent",
                   help="Exit-code policy. 'equivalent' (default): nonzero exit "
                        "unless every case is provable — for validating known-"
                        "equivalent rewrite pairs in CI. 'report-only': always 0.")
    p.add_argument("--json", metavar="FILE", help="Write full results as JSON.")
    p.add_argument("--csv", metavar="FILE", help="Write results as CSV.")
    p.add_argument("--keep", metavar="DIR", default=None,
                   help="Keep intermediate .json/.rkt/.result under DIR "
                        "(default: ephemeral temp dirs, cleaned up).")
    p.add_argument("--no-retry", action="store_true",
                   help="Don't re-run transient failures (panic/timeout/error) "
                        "serially at the end. By default they are retried once "
                        "with no contention, so a heavy case starved under -j "
                        "isn't misreported as a failure.")
    p.add_argument("-v", "--verbose", action="store_true",
                   help="Print each case as it finishes.")
    p.add_argument("-q", "--quiet", action="store_true",
                   help="Only print the final summary.")
    p.add_argument("--no-color", action="store_true", help="Disable ANSI color.")
    p.add_argument("--frontend", help="Path to sqleq-frontend (else $SQLEQ_FRONTEND / "
                                      "PATH / this repo's target/{release,debug}).")
    p.add_argument("--prover", help="Path to qed-prover (else PATH / $QED_PROVER).")
    p.add_argument("--sqlsolver", action="store_true",
                   help="Also ask SQLSolver about every case that lowered, over "
                        "the same Input JSON the QED prover gets. Informational "
                        "only: it never changes the exit code, and its NEQ is not "
                        "a refutation. Needs a JDK and the de-Calcited fork; see "
                        "docs/SQLSOLVER.md.")
    p.add_argument("--sqlsolver-tree", metavar="DIR",
                   help="The SQLSolver fork to run (else $SQLEQ_SQLSOLVER; one "
                        "of the two is required). Its exploded dependency "
                        "directory comes from $SQLEQ_SQLSOLVER_DEPS.")
    p.add_argument("--sqlsolver-timeout", type=int, default=None, metavar="MS",
                   help="Per-row cap for the second opinion, in ms "
                        "(default: --timeout). Its rows run sequentially, so this "
                        "is a per-row budget, not a share of one.")
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    c = Color(on=not args.no_color and sys.stdout.isatty())

    frontend_bin = discover_frontend(args.frontend)
    prover_bin = discover_prover(args.prover)
    # Resolved before a single case runs — including the driver rebuild — so a
    # fork that is missing or will not compile costs a second, not a full pass.
    ss_cp, ss_cwd = (None, None)
    if args.sqlsolver:
        ss_cp, ss_cwd = discover_sqlsolver(args.sqlsolver_tree)

    files = collect_inputs(args.paths)
    if not files:
        print("error: no .sql or .json inputs found.", file=sys.stderr)
        return 2

    root = common_root(files)

    def disp(f: Path) -> str:
        rel = os.path.relpath(str(f.resolve()), str(root))
        return f.name if rel in (".", "") else rel

    ss_timeout_ms = args.sqlsolver_timeout or int(args.timeout * 1000)

    keep_dir = Path(args.keep) if args.keep else None
    if keep_dir:
        keep_dir.mkdir(parents=True, exist_ok=True)

    # One directory for the whole second-opinion pass: a job per case, then the
    # driver's `todo`/`results`. Under --keep it sits beside the kept workdirs,
    # where a refused or surprising row can be replayed by hand.
    ss_dir, ss_tmp = None, None
    if args.sqlsolver:
        if keep_dir:
            # Cleared, not reused, unlike the per-case workdirs beside it: the
            # driver resumes from `results.jsonl`, so a previous run's answers
            # left in place would be reported as this run's without a single row
            # being re-asked.
            ss_dir = (keep_dir / "sqlsolver").resolve()
            shutil.rmtree(ss_dir, ignore_errors=True)
        else:
            ss_tmp = tempfile.mkdtemp(prefix="sqleq-ss-")
            ss_dir = Path(ss_tmp).resolve()
        ss_dir.mkdir(parents=True, exist_ok=True)

    if not args.quiet:
        print(c.dim(f"sqleq-frontend: {frontend_bin}"))
        print(c.dim(f"qed-prover:   {prover_bin}"))
        if ss_cwd:
            print(c.dim(f"sqlsolver:    {ss_cwd}"))
        print(c.bold(f"Checking {len(files)} case(s) "
                     f"with {args.jobs} worker(s), {args.timeout:.0f}s/case…"))
        print()

    name_w = min(60, max((len(disp(f)) for f in files), default=10))
    live = sys.stdout.isatty()
    cases: list[Case] = []
    t0 = time.monotonic()
    done = 0
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as ex:
        futs = {
            ex.submit(run_case, f, disp(f), frontend_bin, prover_bin,
                      args.timeout, args.smt_timeout, keep_dir, ss_dir): f
            for f in files
        }
        for fut in as_completed(futs):
            case = fut.result()
            cases.append(case)
            done += 1
            if args.verbose:
                print_case_line(c, case, name_w)
            elif not args.quiet:
                if case.status != PROVABLE:
                    print_case_line(c, case, name_w)
                elif live:
                    print(c.dim(f"  [{done}/{len(files)}] "), end="\r", flush=True)

    # Retry transient failures serially (no contention) — a heavy case starved
    # or OOM-killed under -j shouldn't be misreported as a real failure. A
    # refusal is deterministic, so it is never retried.
    TRANSIENT = (PANIC, TIMEOUT, ERROR)
    if not args.no_retry:
        retry = [c for c in cases if c.status in TRANSIENT]
        if retry:
            if not args.quiet and live:
                print(" " * 30, end="\r")
            if not args.quiet:
                print(c.dim(f"  re-running {len(retry)} transient "
                            f"failure(s) serially…"))
            idx = {c.name: i for i, c in enumerate(cases)}
            for old in retry:
                new = run_case(Path(old.path), old.name, frontend_bin, prover_bin,
                               args.timeout, args.smt_timeout, keep_dir, ss_dir)
                better = (new.status not in TRANSIENT) or (
                    STATUS_ORDER.index(new.status) < STATUS_ORDER.index(old.status))
                if better:
                    cases[idx[old.name]] = new
                    if not args.quiet:
                        print_case_line(c, new, name_w)

    wall = time.monotonic() - t0

    # After the prover pass and its retries, and timed apart from them: the
    # second opinion is a separate question and must not be able to move the
    # numbers above, nor they it.
    ss_stats: dict = {}
    if args.sqlsolver and ss_dir is not None:
        if not args.quiet:
            if live:
                print(" " * 30, end="\r")
            n = sum(1 for x in cases if x.s_bucket is None)
            print(c.dim(f"  asking SQLSolver about {n} case(s), "
                        f"sequentially, {ss_timeout_ms}ms/row…"))
        t1 = time.monotonic()
        ss_stats = run_second_opinion(cases, ss_dir, ss_cp, ss_cwd, ss_timeout_ms)
        ss_stats["wall_s"] = round(time.monotonic() - t1, 3)

    cases.sort(key=lambda x: (STATUS_ORDER.index(x.status)
                              if x.status in STATUS_ORDER else 99, x.name))

    if not args.quiet and live:
        print(" " * 30, end="\r")  # clear progress line
    print_summary(c, cases, wall)
    print_second_opinion(c, cases, ss_stats)

    meta = {
        "frontend": frontend_bin,
        "prover": prover_bin,
        "jobs": args.jobs,
        "timeout_s": args.timeout,
        "smt_timeout_ms": args.smt_timeout,
        "total": len(cases),
        "wall_s": round(wall, 3),
        "triviality": triviality_split(cases),
    }
    if args.sqlsolver:
        meta["sqlsolver"] = dict(ss_stats, tree=str(ss_cwd),
                                 timeout_ms=ss_timeout_ms)
    if ss_tmp:
        shutil.rmtree(ss_tmp, ignore_errors=True)
    if args.json:
        write_json(args.json, cases, meta)
        if not args.quiet:
            print(c.dim(f"  wrote {args.json}"))
    if args.csv:
        write_csv(args.csv, cases)
        if not args.quiet:
            print(c.dim(f"  wrote {args.csv}"))

    if args.expect == "report-only":
        return 0
    # 'equivalent' policy: every case must be provable — by *our* prover. The
    # second opinion is deliberately not part of the policy: adding an axis must
    # not be able to turn a red CI run green, and its `no-proof` is not a
    # failure to begin with.
    return 0 if all(x.status == PROVABLE for x in cases) else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\ninterrupted", file=sys.stderr)
        sys.exit(130)
