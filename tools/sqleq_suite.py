# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

"""The pinned-pair suite: header grammar, lint, judgement and `--bless`.

A pinned pair is an ordinary pair file — DDL and two statements — whose leading
comment block says what the pair *is* and what each axis said about it:

    -- truth: not-equivalent
    -- expect frontend: emit
    -- expect fuzz: counterexample
    -- expect sqlsolver-rust: no-proof
    -- expect qed: proved !known-unsound
    -- origin: why this pair is here
    -- witness: the instance on which the two sides differ

`truth` is written by a person and nothing here ever writes it. The `expect`
lines are a ratchet: `sqleq_check.py --expect pinned` fails on *any* movement,
an improvement included, and `--bless` rewrites them so the move is reviewed as
a diff. What `--bless` cannot do is pin an answer that contradicts `truth` — a
proof of a non-equivalent pair, a counterexample to an equivalent one. Those
fail every run until the bug is fixed, or until a person marks that one line
`!known-unsound`, which turns it into a strict expected failure: it passes while
the bug reproduces and fails the run that fixes it, so the marker cannot outlive
the bug.

`truth` is stated under a parameter binding. The default, `index`, is the one
every axis but Lean answers under: `$N` on one side is `$N` on the other. A pair
headed `-- binding: gather` states its truth under the gather rule instead (the
`unnest` side's array `$j` is column `j` of the `VALUES` rows; docs/LEAN.md),
which only the Lean axis answers under. An answer can contradict a truth only
when the axis and the pair use the same binding; under the other one it is an
ordinary pin.

This is the logic only; `sqleq_check.py` runs the axes and calls it. Standard
library only, like the harness.
"""

from __future__ import annotations

import os
import re
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

EQUIVALENT = "equivalent"
NOT_EQUIVALENT = "not-equivalent"
TRUTHS = (EQUIVALENT, NOT_EQUIVALENT)

# Canonical order: the order `--bless` inserts missing lines in, and the table's column order.
AXES = ("frontend", "fuzz", "qed", "sqlsolver-rust", "sqlsolver-jvm", "lean")
PROVERS = ("qed", "sqlsolver-rust", "sqlsolver-jvm")

INDEX = "index"
GATHER = "gather"
BINDINGS = (INDEX, GATHER)
# The binding each axis answers under. Only Lean reads a scalar and an array at the same `$N` as
# the gather rule relates them; every other axis refuses such a pair.
AXIS_BINDING = {a: GATHER if a == "lean" else INDEX for a in AXES}

PROVED_WORDS = ("proved", "proved-literal")
# What may be pinned, per axis. Only the stable *kind* of an answer is pinned, never its message,
# so rewording a refusal does not move a pin and changing what is refused does.
WORDS = {
    "frontend": ("emit", "emit-reflexive", "refuse:parse", "refuse:unsupported", "refuse:schema",
                 "refuse:parameter-misaligned"),
    "fuzz": ("counterexample", "no-counterexample", "param-misaligned", "not-comparable",
             "nondet-skip", "no-schema", "no-tables", "error"),
    "qed": PROVED_WORDS + ("no-proof", "no-plan", "panic", "error"),
    "sqlsolver-rust": PROVED_WORDS + ("no-proof", "unsupported", "no-plan", "error"),
    "sqlsolver-jvm": PROVED_WORDS + ("no-proof", "unsupported", "no-plan", "error"),
    # `no-witness` is a kernel proof too, only possibly vacuous, so it is a claim of equivalence.
    "lean": ("proved-gather", "no-witness", "unsupported", "invalid-sql", "error"),
}
# The answers that claim equivalence, per axis, and the one that claims the opposite.
CLAIMS_EQUIVALENT = {**{a: PROVED_WORDS for a in PROVERS}, "lean": ("proved-gather", "no-witness")}
REFUTES = {"fuzz": ("counterexample",)}
# Evidence for an equivalent truth: a claim that is not possibly vacuous, which `no-witness` is.
EVIDENCE_EQUIVALENT = {**CLAIMS_EQUIVALENT, "lean": ("proved-gather",)}
# Never pinnable: each says the run did not get an answer, not what the answer was.
UNPINNABLE = ("timeout", "missing")

CATALOG_FLAGS = {"declared": [], "inferred": ["--infer"], "inferred-seeded": ["--infer-seeded"]}
MARKER = "!known-unsound"
TEXT_KEYS = ("truth", "binding", "catalog", "origin", "witness", "argument")

# A directive is `-- key: value` with a lowercase key right after `-- `. Prose in the header starts
# with a capital or with more indentation, so a typo such as `-- expect fuz:` is a lint error
# instead of a comment nobody reads.
_DIRECTIVE = re.compile(r"^-- ([a-z][a-z0-9-]*(?: [a-z][a-z0-9-]*)?):(?: (.*))?$")


@dataclass
class Pin:
    word: str
    marker: bool
    line: int  # index into the file's lines


@dataclass
class Header:
    truth: Optional[str] = None
    binding: str = INDEX
    catalog: str = "declared"
    text: dict = field(default_factory=dict)      # key -> value, for every TEXT_KEYS key present
    lines: dict = field(default_factory=dict)     # key -> line index, for every directive
    expect: dict = field(default_factory=dict)    # axis -> Pin
    errors: list = field(default_factory=list)


def _block_end(lines: list) -> int:
    """The index of the first line past the leading comment block — the same rule as
    `sqleq-lean/tests/examples.rs`: comment lines and blank lines, up to the first SQL."""
    for i, ln in enumerate(lines):
        if not (ln.startswith("--") or not ln.strip()):
            return i
    return len(lines)


def parse_header(text: str) -> Header:
    """Read the directives in the leading comment block. Grammar errors are collected in
    `errors`, not raised: a malformed case is reported beside the others, not instead of them."""
    h = Header()
    lines = text.splitlines()
    end = _block_end(lines)
    for i, raw in enumerate(lines):
        ln = raw.rstrip()
        m = _DIRECTIVE.match(ln)
        if not m:
            continue
        key, value = m.group(1), (m.group(2) or "").strip()
        if i >= end:
            # A directive below the SQL would be silently ignored, which for a pin is worse than
            # an error: the case would look pinned and check nothing.
            if key in TEXT_KEYS or key.startswith("expect "):
                h.errors.append(f"line {i + 1}: `{key}:` below the first SQL line is not read")
            continue
        if key in h.lines:
            h.errors.append(f"line {i + 1}: duplicate `{key}:`")
            continue
        h.lines[key] = i
        if key.startswith("expect "):
            axis = key.split(" ", 1)[1]
            if axis not in AXES:
                h.errors.append(f"line {i + 1}: unknown axis `{axis}` (one of {', '.join(AXES)})")
                continue
            parts = value.split()
            marker = MARKER in parts
            words = [p for p in parts if p != MARKER]
            if len(words) != 1:
                h.errors.append(f"line {i + 1}: `expect {axis}:` takes one word, then optionally "
                                f"{MARKER}")
                continue
            word = words[0]
            if word not in WORDS[axis]:
                why = ("is never pinnable: it says the run got no answer" if word in UNPINNABLE
                       else f"is not one of {', '.join(WORDS[axis])}")
                h.errors.append(f"line {i + 1}: `{word}` {why}")
                continue
            h.expect[axis] = Pin(word, marker, i)
        elif key in TEXT_KEYS:
            h.text[key] = value
            if key == "truth":
                h.truth = value
            elif key == "catalog":
                h.catalog = value
            elif key == "binding":
                h.binding = value
        else:
            h.errors.append(f"line {i + 1}: unknown directive `{key}:`")
    return h


def contradicts(truth: Optional[str], axis: str, word: str, binding: str = INDEX) -> bool:
    """Whether an answer is impossible for a pair of this truth — a soundness failure of that axis
    (or of the frontend feeding it), as opposed to a capability move. An axis answering under
    another binding than the one the truth is stated under contradicts nothing."""
    if AXIS_BINDING.get(axis) != binding:
        return False
    if truth == NOT_EQUIVALENT:
        return (word in CLAIMS_EQUIVALENT.get(axis, ())
                or (axis == "frontend" and word == "emit-reflexive"))
    if truth == EQUIVALENT:
        return word in REFUTES.get(axis, ())
    return False


def lint(h: Header) -> list:
    """Every grammar error, then the rules that make a case worth having: a truth, a reason for
    being here, evidence for the truth, and no marker that excuses nothing."""
    errs = list(h.errors)
    if h.truth is None:
        errs.append("no `truth:` line")
    elif h.truth not in TRUTHS:
        errs.append(f"`truth: {h.truth}` is not one of {', '.join(TRUTHS)}")
    if h.catalog not in CATALOG_FLAGS:
        errs.append(f"`catalog: {h.catalog}` is not one of {', '.join(CATALOG_FLAGS)}")
    if h.binding not in BINDINGS:
        errs.append(f"`binding: {h.binding}` is not one of {', '.join(BINDINGS)}")
    if not h.text.get("origin"):
        errs.append("no `origin:` line saying why this pair is pinned")
    for axis, pin in h.expect.items():
        contra = contradicts(h.truth, axis, pin.word, h.binding)
        if pin.marker and not contra:
            errs.append(f"`expect {axis}: {pin.word}` carries {MARKER}, but that answer does not "
                        f"contradict `truth: {h.truth}`")
        # --bless never writes one of these, so it was written by hand. Caught here, it fails
        # every run, including the CI runs that do not ask that axis.
        if contra and not pin.marker:
            errs.append(f"`expect {axis}: {pin.word}` contradicts `truth: {h.truth}`; mark it "
                        f"{MARKER} if that is a known bug")
    # Evidence for the truth counts only from an axis answering under the pair's binding.
    def says(table):
        return any(p.word in table.get(a, ()) and not p.marker
                   and AXIS_BINDING[a] == h.binding for a, p in h.expect.items())
    gather = h.binding == GATHER
    if h.truth == NOT_EQUIVALENT and not says(REFUTES) and not h.text.get("witness"):
        errs.append("a non-equivalent pair needs a `witness:`" if gather else
                    "a non-equivalent pair needs `expect fuzz: counterexample` or a `witness:`")
    if h.truth == EQUIVALENT and not says(EVIDENCE_EQUIVALENT) and not h.text.get("argument"):
        errs.append("an equivalent pair needs `expect lean: proved-gather` or an `argument:`"
                    if gather else "an equivalent pair needs a prover's `proved` pin or an "
                    "`argument:`")
    return errs


# Judgement states. Only OK and KNOWN pass.
OK = "ok"                    # the pin holds
KNOWN = "known"              # a pinned !known-unsound answer, still reproducing
CHANGED = "changed"          # the answer moved (either way); --bless takes it
UNPINNED = "unpinned"        # the axis ran and the case has no line for it; --bless adds one
STALE = "stale-marker"       # a !known-unsound line whose bug no longer reproduces; --bless drops it
INVARIANT = "invariant"      # the answer contradicts truth and nobody said that is known
UNANSWERED = "unanswered"    # timeout / missing: there is no answer to pin
PASSING = (OK, KNOWN)
BLESSABLE = (CHANGED, UNPINNED, STALE)


@dataclass
class Judgement:
    axis: str
    state: str
    observed: str
    pin: Optional[Pin]
    note: str = ""

    @property
    def passed(self) -> bool:
        return self.state in PASSING


def judge(h: Header, observed: dict) -> list:
    """Compare what each axis that ran said (`observed`: axis -> (word, note)) with the pins.
    Axes that did not run are not judged at all: their lines are neither checked nor stale."""
    out = []
    for axis in AXES:
        if axis not in observed:
            continue
        word, note = observed[axis]
        pin = h.expect.get(axis)
        if word in UNPINNABLE:
            state = UNANSWERED
        elif contradicts(h.truth, axis, word, h.binding):
            if pin is not None and pin.marker:
                state = KNOWN if pin.word == word else CHANGED
            else:
                state = INVARIANT
        elif pin is None:
            state = UNPINNED
        elif pin.marker:
            state = STALE
        elif pin.word != word:
            state = CHANGED
        else:
            state = OK
        out.append(Judgement(axis, state, word, pin, note))
    return out


def bless_text(text: str, h: Header, judgements: list) -> str:
    """The file with its `expect` lines brought up to date, and nothing else touched.

    Only blessable judgements are written. A line that exists is rewritten in place. A missing
    one goes in canonical axis order among the `expect` lines already there — after the last
    one for an earlier axis, else before the first one for a later axis, else after `truth:` —
    so the order does not depend on which axes were blessed first. A marker survives only while
    its contradiction does, and is never added. Line endings and the final newline are kept, so
    a second bless is a no-op."""
    nl = "\r\n" if "\r\n" in text else "\n"
    lines = text.splitlines()
    trailing = text.endswith(("\n", "\r"))
    rank = {a: i for i, a in enumerate(AXES)}
    at_line = {a: p.line for a, p in h.expect.items()}
    for j in sorted(judgements, key=lambda j: rank[j.axis]):
        if j.state not in BLESSABLE:
            continue
        keep_marker = (j.pin is not None and j.pin.marker
                       and contradicts(h.truth, j.axis, j.observed, h.binding))
        new = f"-- expect {j.axis}: {j.observed}" + (f" {MARKER}" if keep_marker else "")
        if j.pin is not None:
            lines[j.pin.line] = new
            continue
        before = [n for a, n in at_line.items() if rank[a] < rank[j.axis]]
        after = [n for a, n in at_line.items() if rank[a] > rank[j.axis]]
        at = (max(before) + 1 if before else min(after) if after
              else h.lines.get("truth", -1) + 1)
        lines.insert(at, new)
        at_line = {a: n + 1 if n >= at else n for a, n in at_line.items()}
        at_line[j.axis] = at
    out = nl.join(lines)
    return out + nl if trailing else out


def write_if_changed(path: Path, text: str) -> bool:
    """Replace the file atomically, and only when its bytes would change."""
    if path.read_bytes() == text.encode():
        return False
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(text.encode())
        os.chmod(tmp, path.stat().st_mode & 0o7777)
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise
    return True


GLYPH = {OK: "✓", KNOWN: "≈", CHANGED: "✗", UNPINNED: "+", STALE: "✗", INVARIANT: "‼",
         UNANSWERED: "⏱"}


def cell(j: Optional[Judgement]) -> str:
    """One table cell: what the axis said, and how that compares with the pin."""
    if j is None:
        return "·"
    g = GLYPH[j.state]
    if j.state in (CHANGED, STALE) and j.pin is not None:
        return f"{g} {j.pin.word}→{j.observed}"
    return f"{g} {j.observed}"


def explain(j: Judgement, truth: Optional[str]) -> str:
    """The one-line reason a judgement failed, with what to do about it."""
    pinned = j.pin.word if j.pin else None
    if j.state == INVARIANT:
        return (f"{j.axis} says `{j.observed}` on a pair whose truth is `{truth}` — a soundness "
                f"failure; --bless will not pin it. Fix the bug, or mark the line {MARKER} and "
                f"file an issue.")
    if j.state == UNANSWERED:
        return f"{j.axis} gave no answer (`{j.observed}`); shrink the case or raise --timeout."
    if j.state == STALE:
        return (f"{j.axis}: the {MARKER} answer `{pinned}` no longer reproduces (now "
                f"`{j.observed}`); --bless drops the marker.")
    if j.state == UNPINNED:
        return f"{j.axis}: no `expect {j.axis}:` line; --bless adds `{j.observed}`."
    return f"{j.axis}: pinned `{pinned}`, now `{j.observed}`; --bless takes the new answer."
