#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

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
`tools/sqlsolver/` or, with `--sqlsolver-impl=rust`, through this repo's Rust
port of it. It is off by default and it never changes the exit code —
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

`--expect pinned` is the other policy: every case carries its own expected
answer per axis in its header (`tests/pairs/README.md`), `--axes` picks which
axes run — `sqleq-fuzz` among them — and any movement fails. The grammar and the
judgement live in `sqleq_suite.py`.

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

import sqleq_suite as suite

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


def discover_fuzz(override: Optional[str]) -> str:
    """Resolve sqleq-fuzz: explicit override -> $SQLEQ_FUZZ -> PATH -> this repo's own
    build (release preferred over debug)."""
    for c in (override, os.environ.get("SQLEQ_FUZZ")):
        if c:
            if os.path.isfile(c) and os.access(c, os.X_OK):
                return c
            sys.exit(f"error: sqleq-fuzz not found or not executable at: {c}")
    found = shutil.which("sqleq-fuzz") or _newest(
        [str(REPO / "target" / p / "sqleq-fuzz") for p in ("release", "debug")])
    if found:
        return found
    sys.exit(
        "error: could not find 'sqleq-fuzz'. Build it with "
        "`cargo build --release -p sqleq-fuzz`, put it on PATH, or pass "
        "--fuzz-bin/$SQLEQ_FUZZ.")


# The crate whose sources each binary is built from, for `stale_build`.
CRATE_DIR = {"sqleq-frontend": REPO, "sqleq-fuzz": REPO / "sqleq-fuzz",
             "sqleq-solver": REPO / "sqleq-solver"}


def stale_build(binary: str) -> Optional[str]:
    """Why a binary built in this repo's `target/` is older than what it was built from, or None.

    A pin blessed against a stale build records what an older tree said, and the next fresh
    build reports it as a regression nobody made. Only binaries under this repo's `target/` are
    judged; one from anywhere else has no sources here to compare with."""
    path = Path(binary).resolve()
    try:
        path.relative_to((REPO / "target").resolve())
    except ValueError:
        return None
    crate = CRATE_DIR.get(path.name)
    if crate is None:
        return None
    srcs = [REPO / "Cargo.lock", crate / "Cargo.toml"] + list((crate / "src").rglob("*.rs"))
    newest = max((f for f in srcs if f.is_file()), key=lambda f: f.stat().st_mtime, default=None)
    if newest is not None and newest.stat().st_mtime > path.stat().st_mtime:
        return f"{binary} is older than {os.path.relpath(newest, REPO)}"
    return None


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
LOWERED = "lowered"          # lowered, and the qed axis was not asked (see --axes)

STATUS_ORDER = [PROVABLE, UNPROVABLE, TIMEOUT, REFUSED, PANIC, ERROR, LOWERED]

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
    refuse_kind: str = ""        # parse | unsupported | schema | parameter-misaligned
    lowered: bool = False        # the frontend produced a plan (or the input was one)
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
    # The Lean axis, only when --lean is on: sqleq-lean's own verdict on the pair
    # file (it reads the .sql itself, so it answers whatever the frontend did).
    l_verdict: Optional[str] = None
    l_reason: str = ""
    l_shape: str = ""
    l_ms: Optional[int] = None
    # The sqleq-fuzz axis, only when `fuzz` is among --axes: its label's kind (the part
    # before the first `:`), the counterexample or the reason, and its wall time.
    f_verdict: Optional[str] = None
    f_note: str = ""
    f_ms: Optional[int] = None


def classify_refusal(err: str) -> tuple[str, str]:
    """Map the frontend's stderr to (refuse_kind, one-line reason).

    The four kinds mirror FrontendError: a `PARSE ERROR:` prefix means sqlparser
    rejected the text, `unsupported:` means we declined to lower a construct,
    `parameter-misaligned:` means the two queries' `$N` do not line up, and
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
    if reason.startswith("parameter-misaligned:"):
        return "parameter-misaligned", reason
    return "schema", reason


def catalog_flags(src: Path) -> list:
    """The frontend flags a `.sql` case's `-- catalog:` header asks for (none when absent).

    A pair whose queries use `$N` needs an inferred catalog: under the default, declared one
    the frontend refuses a bare placeholder. An unknown value raises, rather than silently
    lowering against a catalog the case did not ask for."""
    if src.suffix != ".sql":
        return []
    h = suite.parse_header(src.read_text())
    if h.catalog not in suite.CATALOG_FLAGS:
        raise ValueError(f"unknown `catalog: {h.catalog}`")
    return suite.CATALOG_FLAGS[h.catalog]


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
    prover: Optional[str],
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
            try:
                flags = catalog_flags(src)
            except ValueError as e:
                case.message = str(e)
                case.wall = time.monotonic() - t0
                return case
            fr = run_cmd([frontend] + flags + [local_sql.name, json_path.name], workdir,
                         case_timeout)
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

        case.lowered = True
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

        if prover is None:
            case.status = LOWERED
            case.wall = time.monotonic() - t0
            return case

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


@dataclass
class SsDriver:
    """How to run the second opinion's driver: the command up to its positional arguments,
    where to run it, and with what environment. Both implementations take the same
    `<jobs> <results> --timeout-ms=N` and write the same rows, so everything after the
    command is shared."""
    impl: str
    cmd: list
    cwd: Path
    env: dict
    where: str


def discover_sqlsolver_rust(override: Optional[str]) -> SsDriver:
    """Resolve the Rust port's driver: explicit override -> $SQLEQ_SOLVER_BIN -> PATH ->
    this repo's own build (release preferred over debug).

    No JDK, no fork tree and no library path: Z3 is linked at build time and its
    location baked into the binary (`sqleq-solver/build.rs`)."""
    for c in (override, os.environ.get("SQLEQ_SOLVER_BIN")):
        if c:
            if os.path.isfile(c) and os.access(c, os.X_OK):
                return SsDriver("rust", [c], REPO, dict(os.environ), c)
            sys.exit(f"error: sqleq-solver not found or not executable at: {c}")
    found = shutil.which("sqleq-solver") or _newest(
        [str(REPO / "target" / p / "sqleq-solver") for p in ("release", "debug")])
    if found:
        return SsDriver("rust", [found], REPO, dict(os.environ), found)
    sys.exit(
        "error: could not find 'sqleq-solver'. Build it with "
        "`cargo build --release -p sqleq-solver` (it links Z3; see "
        "sqleq-solver/build.rs), put it on PATH, or pass "
        "--sqlsolver-bin/$SQLEQ_SOLVER_BIN.")


def discover_sqlsolver_jvm(override: Optional[str]) -> SsDriver:
    """The JVM driver (`tools/sqlsolver/IrDriver.java`) over the de-Calcited fork."""
    cp, tree = discover_sqlsolver(override)
    return SsDriver(
        "jvm",
        ["java", f"-Djava.library.path={tree / 'lib'}", "-cp", cp, "IrDriver"],
        tree, dict(os.environ, LD_LIBRARY_PATH=str(tree / "lib")), str(tree))


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


def run_second_opinion(cases: list[Case], ss_dir: Path, driver: SsDriver,
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
    passes, halts, stall = 0, 0, None
    while True:
        have = _ss_answered(out_path)
        todo = [j for j in jobs if j["name"] not in have]
        if not todo:
            break
        todo_path.write_text("".join(json.dumps(j) + "\n" for j in todo))
        passes += 1
        proc = subprocess.run(
            driver.cmd + [str(todo_path), str(out_path), f"--timeout-ms={timeout_ms}"],
            cwd=str(driver.cwd), env=driver.env, capture_output=True, text=True)
        # Exit 3 is the driver taking its own process down because a row would not
        # stop (the JVM's interrupt missed; the Rust driver's grace period ran out).
        # It writes the row first, so resuming always advances.
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
# The Lean axis
# ---------------------------------------------------------------------------
#
# `sqleq-lean` decides one class the frontend refuses outright, `INSERT … VALUES`
# against `INSERT … SELECT * FROM unnest(…)`, under the gather rule (see
# docs/LEAN.md). It parses the pair file itself, so it runs over every `.sql`
# case regardless of what the frontend made of it, and like `--sqlsolver` it is
# a second opinion that never moves the exit code.

LEAN_ORDER = ["proved-gather", "no-witness", "unsupported", "invalid-sql",
              "error", "timeout", "missing"]


def discover_lean(override: Optional[str]) -> str:
    for cand in (override, os.environ.get("SQLEQ_LEAN"),
                 str(REPO / "target" / "release" / "sqleq-lean"),
                 str(REPO / "target" / "debug" / "sqleq-lean")):
        if cand and Path(cand).is_file() and os.access(cand, os.X_OK):
            return str(Path(cand).resolve())
    sys.exit("error: --lean needs the sqleq-lean binary: `cargo build --release -p "
             "sqleq-lean` (it also needs a Lean toolchain on PATH), or pass "
             "--lean-bin / set $SQLEQ_LEAN.")


def run_lean(cases: list[Case], lean_bin: str, jobs: int, timeout_s: float,
             keep_dir: Optional[Path]) -> dict:
    """Run sqleq-lean over every `.sql` case and attach its verdicts in place."""
    todo = [x for x in cases if x.path.endswith(".sql")]
    if not todo:
        return {"rows": 0}
    with tempfile.TemporaryDirectory(prefix="sqleq-lean-") as tmp:
        out = Path(tmp) / "lean.json"
        cmd = [lean_bin, "--full-names", "--json", str(out), "--jobs", str(max(1, jobs)),
               "--timeout", str(max(60, int(timeout_s * 10)))]
        if keep_dir is not None:
            cmd += ["--keep", str((keep_dir / "lean").resolve())]
        t0 = time.monotonic()
        proc = subprocess.run(cmd + [x.path for x in todo], capture_output=True, text=True)
        wall = time.monotonic() - t0
        try:
            got = json.loads(out.read_text())
        except (OSError, ValueError):
            for x in todo:
                x.l_verdict, x.l_reason = "missing", _tail(proc.stderr) or "no output"
            return {"rows": len(todo), "wall_s": wall, "failed": _tail(proc.stderr)}
    for x in todo:
        rec = got.get(x.path)
        if rec is None:
            x.l_verdict, x.l_reason = "missing", "no record"
            continue
        x.l_verdict = rec.get("verdict")
        x.l_reason = rec.get("reason", "")
        x.l_shape = rec.get("shape", "")
        x.l_ms = rec.get("ms")
    return {"rows": len(todo), "wall_s": wall}


def print_lean(c: Color, cases: list[Case], stats: dict):
    scored = [x for x in cases if x.l_verdict is not None]
    if not scored:
        return
    counts: dict = {}
    for x in scored:
        counts[x.l_verdict] = counts.get(x.l_verdict, 0) + 1
    print()
    print(c.bold("  Lean axis") + c.dim("  — INSERT … VALUES vs INSERT … SELECT * FROM unnest(…)"))
    print(c.dim("  " + "─" * 40))
    for v in LEAN_ORDER + sorted(set(counts) - set(LEAN_ORDER)):
        if counts.get(v):
            print(f"  {v:<22} {counts[v]:>5}")
    print(c.dim("  " + "─" * 40))
    for x in scored:
        if x.l_verdict == "proved-gather":
            print(c.dim(f"  {'proved-gather':<22}       {x.name}"))
    if stats.get("wall_s") is not None:
        print(c.dim(f"  {'wall time':<22} {stats['wall_s']:.2f}s"))
    print(c.dim("  note  `proved-gather` is proved under the gather rule: the unnest\n"
                "        side's array $j is column j of the VALUES rows. It is not the\n"
                "        same-$N claim `provable` makes. See docs/LEAN.md."))


# ---------------------------------------------------------------------------
# The fuzz axis
# ---------------------------------------------------------------------------
#
# `sqleq-fuzz` is the only axis that can refute: it runs both statements on random
# instances in DuckDB and compares the results. It reads the pair file itself and
# binds `$N` on its own, so it needs no frontend and ignores the catalog header.
# The trial budget is always passed explicitly — the tool's defaults are free to
# change, and a pinned `no-counterexample` is only a claim about one budget.

FUZZ_ARGS = ["--trials", "120", "--rows", "5", "--seed", "0"]

# The label's kind is the part before the first `:` (`ERROR:…`, `PARAM-MISALIGNED:…`).
_FUZZ_WORD = {
    "NOT-EQUIVALENT": "counterexample", "NO-COUNTEREXAMPLE": "no-counterexample",
    "PARAM-MISALIGNED": "param-misaligned", "NOT-COMPARABLE": "not-comparable",
    "NONDET-SKIP": "nondet-skip", "NO-SCHEMA": "no-schema", "NO-TABLES": "no-tables",
    "ERROR": "error",
}


def fuzz_one(fuzz_bin: str, path: str, timeout_s: float) -> tuple:
    """(word, note, ms) for one pair file."""
    path = os.path.abspath(path)
    r = run_cmd([fuzz_bin, "file", path] + FUZZ_ARGS, os.path.dirname(path), timeout_s)
    ms = int(r.wall * 1000)
    if r.timed_out:
        return "timeout", "", ms
    lines = [ln.strip() for ln in r.out.splitlines() if ln.strip()]
    if r.rc != 0 or not lines:
        return "error", _tail(r.err) or f"sqleq-fuzz exit {r.rc}", ms
    label = lines[0]
    word = _FUZZ_WORD.get(label.split(":", 1)[0], "error")
    note = next((ln[len("counterexample: "):] for ln in lines[1:]
                 if ln.startswith("counterexample: ")), "")
    if not note and ":" in label:
        note = label.split(":", 1)[1].strip()
    return word, note, ms


def run_fuzz(cases: list[Case], fuzz_bin: str, jobs: int, timeout_s: float) -> dict:
    """Run sqleq-fuzz over every `.sql` case and attach its verdicts in place."""
    todo = [x for x in cases if x.path.endswith(".sql")]
    t0 = time.monotonic()
    with ThreadPoolExecutor(max_workers=max(1, jobs)) as ex:
        futs = {ex.submit(fuzz_one, fuzz_bin, x.path, timeout_s): x for x in todo}
        for fut in as_completed(futs):
            x = futs[fut]
            x.f_verdict, x.f_note, x.f_ms = fut.result()
    return {"rows": len(todo), "wall_s": round(time.monotonic() - t0, 3)}


def print_fuzz(c: Color, cases: list[Case], stats: dict):
    scored = [x for x in cases if x.f_verdict is not None]
    if not scored:
        return
    counts: dict = {}
    for x in scored:
        counts[x.f_verdict] = counts.get(x.f_verdict, 0) + 1
    print()
    print(c.bold("  Fuzz axis") + c.dim("  — sqleq-fuzz, random instances in DuckDB"))
    print(c.dim("  " + "─" * 40))
    for v in sorted(counts):
        print(f"  {v:<22} {counts[v]:>5}")
    print(c.dim("  " + "─" * 40))
    for x in scored:
        if x.f_verdict == "counterexample":
            print(c.dim(f"  {'counterexample':<22}       {x.name}"))
    if stats.get("wall_s") is not None:
        print(c.dim(f"  {'wall time':<22} {stats['wall_s']:.2f}s"))
    print(c.dim("  note  `no-counterexample` is not a proof: it is the verdict of "
                f"{FUZZ_ARGS[1]} trials\n        over a small value domain "
                "(see sqleq-fuzz/README.md)."))


# ---------------------------------------------------------------------------
# Pinned mode
# ---------------------------------------------------------------------------
#
# Each axis's answer, reduced to the one word `sqleq_suite.WORDS` lets a case pin.
# A prover asked about a pair the frontend refused has nothing to say about it,
# and says `no-plan` — the refusal itself is the frontend axis's answer.

_QED_WORD = {PROVABLE: "proved", UNPROVABLE: "no-proof", PANIC: "panic", TIMEOUT: "timeout",
             ERROR: "error"}


def observe(case: Case, axes: list) -> dict:
    """axis -> (word, note) for every axis in `axes` that this run asked about the case."""
    out = {}
    if "frontend" in axes:
        if case.lowered:
            out["frontend"] = ("emit-reflexive" if case.trivial else "emit", "")
        elif case.status == REFUSED:
            out["frontend"] = (f"refuse:{case.refuse_kind}", case.message)
        elif case.status == TIMEOUT:
            out["frontend"] = ("timeout", case.message)
        else:
            out["frontend"] = ("missing", case.message)
    no_plan = not case.lowered
    if "qed" in axes:
        if no_plan:
            out["qed"] = ("no-plan", "")
        else:
            word = _QED_WORD.get(case.status, "error")
            if word == "proved" and case.trivial:
                word = "proved-literal"
            out["qed"] = (word, case.message)
    for axis in ("sqlsolver-rust", "sqlsolver-jvm"):
        if axis in axes:
            out[axis] = (("no-plan", "") if no_plan
                         else (case.s_bucket or SQLSOLVER_MISSING, case.s_note))
    if "fuzz" in axes:
        out["fuzz"] = (case.f_verdict or "missing", case.f_note)
    return out


@dataclass
class Pinned:
    case: Case
    header: suite.Header
    lint: list
    judgements: list

    @property
    def passed(self) -> bool:
        return not self.lint and all(j.passed for j in self.judgements)


def judge_cases(cases: list[Case], axes: list) -> list:
    out = []
    for x in cases:
        h = suite.parse_header(Path(x.path).read_text())
        errs = suite.lint(h)
        out.append(Pinned(x, h, errs, [] if errs else suite.judge(h, observe(x, axes))))
    return out


def bless(pinned: list) -> list:
    """Rewrite the `expect` lines of every lint-clean case; the names of the files changed."""
    changed = []
    for p in pinned:
        if p.lint:
            continue
        path = Path(p.case.path)
        text = path.read_text()
        if suite.write_if_changed(path, suite.bless_text(text, p.header, p.judgements)):
            changed.append(p.case.name)
    return changed


def print_pinned(c: Color, pinned: list, axes: list):
    cols = [a for a in suite.AXES if a in axes]
    name_w = min(64, max((len(p.case.name) for p in pinned), default=10))
    truth = {suite.EQUIVALENT: "EQ", suite.NOT_EQUIVALENT: "NEQ"}
    grid = []
    for p in pinned:
        by = {j.axis: j for j in p.judgements}
        grid.append([p.case.name, truth.get(p.header.truth, "?")]
                    + (["lint"] * len(cols) if p.lint else [suite.cell(by.get(a)) for a in cols]))
    widths = [name_w, 5] + [max([len(a)] + [len(r[2 + i]) for r in grid])
                            for i, a in enumerate(cols)]
    print()
    print(c.bold("  " + "  ".join(h.ljust(w) for h, w in zip(["case", "truth"] + cols, widths))))
    print(c.dim("  " + "─" * (sum(widths) + 2 * len(widths))))
    for row, p in zip(grid, pinned):
        line = "  ".join(v.ljust(w) for v, w in zip(row, widths))
        print("  " + (line if p.passed else c.red(line)))
    failed = [p for p in pinned if not p.passed]
    print(c.dim("  " + "─" * (sum(widths) + 2 * len(widths))))
    print(c.dim("  ✓ pin holds  ≈ known-unsound, still reproducing  ✗ moved  + unpinned  "
                "‼ contradicts truth  ⏱ no answer  · axis not run"))
    if failed:
        print()
        for p in failed:
            print(c.bold(f"  {p.case.name}"))
            for e in p.lint:
                print(c.red(f"    lint: {e}"))
            for j in p.judgements:
                if not j.passed:
                    print(c.red(f"    {suite.explain(j, p.header.truth)}"))
                    if j.note:
                        print(c.dim(f"      {j.note}"))
    held = len(pinned) - len(failed)
    print()
    print(f"  {c.bold('pinned')} {held}/{len(pinned)} case(s) hold on {', '.join(cols)}")


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
    LOWERED: ("·", "dim"),
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


def print_summary(c: Color, cases: list[Case], wall: float, qed: bool = True):
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
    # Without the qed axis nothing was proved or left unproved, and a `proved 0/N`
    # line would read as a prover that failed everything.
    if qed:
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
            "s_bucket", "s_verdict", "s_ms", "s_note",
            "l_verdict", "l_reason", "l_shape", "l_ms",
            "f_verdict", "f_note", "f_ms"]
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
            "  2  usage / setup error (a missing tool, a bad flag)\n"
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
    p.add_argument("--expect", choices=["equivalent", "report-only", "pinned"],
                   default="equivalent",
                   help="Exit-code policy. 'equivalent' (default): nonzero exit "
                        "unless every case is provable — for validating known-"
                        "equivalent rewrite pairs in CI. 'report-only': always 0. "
                        "'pinned': every case's header pins each axis's answer, and "
                        "any movement fails (tests/pairs/README.md).")
    p.add_argument("--axes", metavar="LIST",
                   help="Comma-separated axes to run: frontend, fuzz, qed, sqlsolver-rust, "
                        "sqlsolver-jvm (default: frontend,qed). A prover axis brings in "
                        "frontend; at most one SQLSolver per run.")
    p.add_argument("--bless", action="store_true",
                   help="With --expect pinned: rewrite each case's `expect` lines for the "
                        "axes that ran. Never pins an answer that contradicts the case's "
                        "truth, nor a timeout.")
    p.add_argument("--fuzz-bin", metavar="PATH",
                   help="Path to sqleq-fuzz (else $SQLEQ_FUZZ / PATH / this repo's "
                        "target/{release,debug}).")
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
    p.add_argument("--sqlsolver-impl", choices=("jvm", "rust"), default="jvm",
                   help="Which SQLSolver to ask: the JVM fork through "
                        "tools/sqlsolver/IrDriver (default), or this repo's Rust "
                        "port, sqleq-solver. Same jobs, same result rows, same "
                        "buckets; the Rust port needs no JDK.")
    p.add_argument("--sqlsolver-tree", metavar="DIR",
                   help="With --sqlsolver-impl=jvm: the SQLSolver fork to run (else "
                        "$SQLEQ_SQLSOLVER; one of the two is required). Its "
                        "exploded dependency directory comes from "
                        "$SQLEQ_SQLSOLVER_DEPS.")
    p.add_argument("--sqlsolver-bin", metavar="PATH",
                   help="With --sqlsolver-impl=rust: the sqleq-solver binary (else "
                        "$SQLEQ_SOLVER_BIN / PATH / this repo's "
                        "target/{release,debug}).")
    p.add_argument("--lean", action="store_true",
                   help="Also run the Lean axis (sqleq-lean) over the .sql cases: "
                        "INSERT ... VALUES vs INSERT ... SELECT * FROM unnest(..) "
                        "pairs, proved under the gather rule. Never changes the exit code.")
    p.add_argument("--lean-bin", metavar="PATH",
                   help="Path to sqleq-lean (else $SQLEQ_LEAN / target/{release,debug}).")
    p.add_argument("--sqlsolver-timeout", type=int, default=None, metavar="MS",
                   help="Per-row cap for the second opinion, in ms "
                        "(default: --timeout). Its rows run sequentially, so this "
                        "is a per-row budget, not a share of one.")
    return p


def resolve_axes(args) -> list:
    """The axes this run asks, in canonical order. The legacy flags still work: `--sqlsolver`
    adds the SQLSolver axis its `--sqlsolver-impl` names, so an old invocation runs what it
    always ran. A prover is handed the frontend's plan, so asking one asks the frontend."""
    if args.axes is None:
        axes = {"frontend", "qed"}
    else:
        axes = {a.strip() for a in args.axes.split(",") if a.strip()}
        unknown = sorted(axes - set(suite.AXES))
        if unknown:
            sys.exit(f"error: unknown axis {', '.join(unknown)} (one of {', '.join(suite.AXES)})")
    if args.sqlsolver:
        axes.add(f"sqlsolver-{args.sqlsolver_impl}")
    if axes & set(suite.PROVERS):
        axes.add("frontend")
    if {"sqlsolver-rust", "sqlsolver-jvm"} <= axes:
        sys.exit("error: one SQLSolver per run; --bless only touches the axes that ran, so "
                 "two runs combine.")
    if not axes:
        sys.exit("error: --axes names no axis")
    return [a for a in suite.AXES if a in axes]


def setup(args) -> dict:
    """Every check that can fail before a case runs. Exits with a message, which `main`
    turns into exit code 2: a missing tool is not a failed case."""
    axes = resolve_axes(args)
    pinned = args.expect == "pinned"
    if args.bless and not pinned:
        sys.exit("error: --bless needs --expect pinned")
    if args.expect == "equivalent" and "qed" not in axes:
        sys.exit("error: --expect equivalent is a policy on the qed axis, which --axes leaves "
                 "out; use --expect pinned or report-only")
    if pinned and args.lean:
        sys.exit("error: the Lean axis pins its own pairs (examples/lean, `-- expect:`); "
                 "--lean does not combine with --expect pinned")
    env = {"axes": axes}
    env["frontend"] = discover_frontend(args.frontend) if "frontend" in axes else None
    env["prover"] = discover_prover(args.prover) if "qed" in axes else None
    # Resolved before a single case runs — including the driver rebuild — so a
    # fork that is missing or will not compile costs a second, not a full pass.
    env["ss"] = None
    if "sqlsolver-rust" in axes:
        env["ss"] = discover_sqlsolver_rust(args.sqlsolver_bin)
    elif "sqlsolver-jvm" in axes:
        env["ss"] = discover_sqlsolver_jvm(args.sqlsolver_tree)
    env["fuzz"] = discover_fuzz(args.fuzz_bin) if "fuzz" in axes else None
    env["lean"] = discover_lean(args.lean_bin) if args.lean else None

    files = collect_inputs(args.paths)
    if not files:
        sys.exit("error: no .sql or .json inputs found.")
    if pinned:
        plans = [str(f) for f in files if f.suffix == ".json"]
        if plans:
            sys.exit("error: --expect pinned reads each case's header, and a .json plan has "
                     "none: " + ", ".join(plans[:3]))
    env["files"] = files

    bins = [env["frontend"], env["fuzz"]]
    if env["ss"] is not None and env["ss"].impl == "rust":
        bins.append(env["ss"].where)
    for b in bins:
        why = b and stale_build(b)
        if why and args.bless:
            sys.exit(f"error: {why}; rebuild it before blessing, or the pins record an "
                     f"older tree's answers")
        if why:
            print(f"warning: {why}; its answers may not be this tree's", file=sys.stderr)
    return env


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    c = Color(on=not args.no_color and sys.stdout.isatty())
    try:
        env = setup(args)
    except SystemExit as e:
        if isinstance(e.code, str):
            print(e.code, file=sys.stderr)
            return 2
        raise
    axes, files = env["axes"], env["files"]
    frontend_bin, prover_bin, ss_driver = env["frontend"], env["prover"], env["ss"]
    fuzz_bin, lean_bin = env["fuzz"], env["lean"]
    pinned_mode = args.expect == "pinned"

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
    if ss_driver is not None:
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
        if frontend_bin:
            print(c.dim(f"sqleq-frontend: {frontend_bin}"))
        if prover_bin:
            print(c.dim(f"qed-prover:   {prover_bin}"))
        if ss_driver:
            print(c.dim(f"sqlsolver:    {ss_driver.where} ({ss_driver.impl})"))
        if fuzz_bin:
            print(c.dim(f"sqleq-fuzz:   {fuzz_bin}"))
        print(c.bold(f"Checking {len(files)} case(s) on {', '.join(axes)} "
                     f"with {args.jobs} worker(s), {args.timeout:.0f}s/case…"))
        print()

    name_w = min(60, max((len(disp(f)) for f in files), default=10))
    live = sys.stdout.isatty()
    cases: list[Case] = []
    t0 = time.monotonic()
    done = 0
    if frontend_bin is None:
        # Only axes that read the pair file themselves: nothing to lower.
        cases = [Case(name=disp(f), path=str(f), status=LOWERED) for f in files]
    else:
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
                if args.verbose and not pinned_mode:
                    print_case_line(c, case, name_w)
                elif not args.quiet and not pinned_mode and case.status not in (PROVABLE,
                                                                                LOWERED):
                    print_case_line(c, case, name_w)
                elif not args.quiet and live:
                    print(c.dim(f"  [{done}/{len(files)}] "), end="\r", flush=True)

    # Retry transient failures serially (no contention) — a heavy case starved
    # or OOM-killed under -j shouldn't be misreported as a real failure. A
    # refusal is deterministic, so it is never retried.
    TRANSIENT = (PANIC, TIMEOUT, ERROR)
    if not args.no_retry and frontend_bin is not None:
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
                    if not args.quiet and not pinned_mode:
                        print_case_line(c, new, name_w)

    wall = time.monotonic() - t0

    # After the prover pass and its retries, and timed apart from them: the
    # second opinion is a separate question and must not be able to move the
    # numbers above, nor they it.
    ss_stats: dict = {}
    if ss_driver is not None and ss_dir is not None:
        if not args.quiet:
            if live:
                print(" " * 30, end="\r")
            n = sum(1 for x in cases if x.s_bucket is None)
            print(c.dim(f"  asking SQLSolver about {n} case(s), "
                        f"sequentially, {ss_timeout_ms}ms/row…"))
        t1 = time.monotonic()
        ss_stats = run_second_opinion(cases, ss_dir, ss_driver, ss_timeout_ms)
        ss_stats["wall_s"] = round(time.monotonic() - t1, 3)

    # The fuzz axis reads the pair files itself, so it is independent of the
    # passes above — though not of the pair, which is the point.
    fuzz_stats: dict = {}
    if fuzz_bin:
        if not args.quiet:
            if live:
                print(" " * 30, end="\r")
            n = sum(1 for x in cases if x.path.endswith(".sql"))
            print(c.dim(f"  asking sqleq-fuzz about {n} .sql case(s)…"))
        fuzz_stats = run_fuzz(cases, fuzz_bin, args.jobs, args.timeout)

    # The Lean axis, likewise apart from both: it reads the pair files itself.
    lean_stats: dict = {}
    if lean_bin:
        if not args.quiet:
            if live:
                print(" " * 30, end="\r")
            n = sum(1 for x in cases if x.path.endswith(".sql"))
            print(c.dim(f"  asking sqleq-lean about {n} .sql case(s)…"))
        lean_stats = run_lean(cases, lean_bin, args.jobs, args.timeout, keep_dir)

    if pinned_mode:
        cases.sort(key=lambda x: x.name)
    else:
        cases.sort(key=lambda x: (STATUS_ORDER.index(x.status)
                                  if x.status in STATUS_ORDER else 99, x.name))

    if not args.quiet and live:
        print(" " * 30, end="\r")  # clear progress line
    pinned: list = []
    blessed: list = []
    if pinned_mode:
        pinned = judge_cases(cases, axes)
        if args.bless:
            blessed = bless(pinned)
            # Judged again from the rewritten files, so what is printed and what decides
            # the exit code are the pins as they now stand.
            pinned = judge_cases(cases, axes)
        print_pinned(c, pinned, axes)
        if args.bless:
            print(f"  {c.bold('blessed')} {len(blessed)} file(s)")
            for name in blessed:
                print(c.dim(f"    {name}"))
    else:
        if frontend_bin is not None:
            print_summary(c, cases, wall, qed="qed" in axes)
        print_second_opinion(c, cases, ss_stats)
        print_fuzz(c, cases, fuzz_stats)
        print_lean(c, cases, lean_stats)

    meta = {
        "axes": axes,
        "frontend": frontend_bin,
        "prover": prover_bin,
        "jobs": args.jobs,
        "timeout_s": args.timeout,
        "smt_timeout_ms": args.smt_timeout,
        "total": len(cases),
        "wall_s": round(wall, 3),
        "triviality": triviality_split(cases),
    }
    if ss_driver is not None:
        meta["sqlsolver"] = dict(ss_stats, impl=ss_driver.impl, where=ss_driver.where,
                                 timeout_ms=ss_timeout_ms)
    if fuzz_bin:
        meta["fuzz"] = dict(fuzz_stats, bin=fuzz_bin, args=FUZZ_ARGS)
    if lean_bin:
        meta["lean"] = dict(lean_stats, bin=lean_bin)
    if pinned_mode:
        meta["pinned"] = {"held": sum(p.passed for p in pinned), "total": len(pinned),
                          "blessed": blessed}
        meta["findings"] = (
            [{"case": p.case.name, "lint": e} for p in pinned for e in p.lint]
            + [{"case": p.case.name, "axis": j.axis, "state": j.state, "observed": j.observed,
                "pinned": j.pin.word if j.pin else None, "note": j.note}
               for p in pinned for j in p.judgements if not j.passed])
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
    if pinned_mode:
        # Blessing never clears an invariant, a timeout or a lint error, so a bless run
        # that leaves one of those behind still fails.
        return 0 if all(p.passed for p in pinned) else 1
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
