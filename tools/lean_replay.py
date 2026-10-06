#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

"""Re-run sqleq-lean's INSERT pairs on a real Postgres, as an independent check of the Lean axis.

`sqleq-lean --replay-plan plan.json` writes, for every pair it proved (`proved-gather`,
`proved-gather-generated`) or proved without a witness (`no-witness`, `no-witness-generated`), the
DDL, both statements, the VALUES rows and the target table's column types and defaults. This script
replays each pair on Postgres and asks two questions.

1. **Does the witness hold?** The Lean witness model says whether the VALUES side's canonical run
   (every parameter a distinct non-NULL value, on an empty table) succeeds. Postgres runs the same
   binding. Agreement is expected in both directions: `proved-gather` should succeed here, and
   `no-witness` should fail.

2. **Do the two sides agree?** The VALUES side runs under the canonical binding, and the unnest side
   under the gather binding built from the same values. They must end with the same outcome
   (success or error), row count, `RETURNING` rows in order, and table contents. That is checked
   twice: once on the empty table, and once with the statement run twice, so the second run meets
   the first run's rows and exercises the conflict clause against existing rows.

Every run is its own transaction, with the DDL applied inside it and rolled back afterwards, so
sequences start fresh each time. That is only true because every sequence a run uses is created
inside it, by the DDL or by this script: sequences are not transactional, so run this against a
database where none of them already exists. Clocks are fixed and random generators drawn from
sequences of their own (see `determinise`). Columns the INSERT omits whose default this does not
recognise as deterministic are left out of the table comparison.

**Generated cells.** For a `proved-gather-generated` or `no-witness-generated` pair, the unnest side
needs the values the VALUES side's generated cells evaluate to, which the claim takes as given. A
probe learns them first (see `probe_script`), and the unnest side is then run under the gather of
the canonical binding with those values filled in, one set per execution. Sequences the VALUES side
advances and the unnest side does not are outside the claim, and the table comparison does not
look at them.

It needs `psql` and a server it can reach. Nothing is written to the database outside the rolled
back transactions.

    python3 tools/lean_replay.py --plan plan.json --json replay.json --host /tmp/sock --port 5432

Statuses, per pair:
- `confirmed` — proved-gather: Postgres agrees the VALUES side succeeds, and the sides agree.
- `confirmed-fails` — no-witness: the VALUES side fails in Postgres too, and the sides agree.
- `witness-disagrees` — Postgres and the Lean witness model disagree on whether the VALUES side
  succeeds. A model gap. Not a soundness failure, since a proof does not rest on the model, but it
  affects credit.
- `ALARM-sides-differ` — the two sides end differently. The one outcome the Lean axis must never
  produce. Investigate before reporting anything.
- `inconclusive: <why>` — the replay could not be set up (DDL Postgres rejects, a type this script
  cannot generate values for, a probe that failed, …). `inconclusive: probe disagrees with the
  VALUES side` is a pair whose two sides differ, but whose VALUES side inserted other generated
  values than the probe found, so the replay, not the theorem, is what is wrong.
"""
from __future__ import annotations

import argparse
import concurrent.futures as cf
import datetime
import json
import os
import re
import subprocess
import sys

# ---------------------------------------------------------------------------------------------------
# Canonical values, one per parameter: distinct, non-NULL, and of the column's type.

_B36 = "0123456789abcdefghijklmnopqrstuvwxyz"


def _base36(n: int) -> str:
    s = ""
    while True:
        n, r = divmod(n, 36)
        s = _B36[r] + s
        if n == 0:
            return s


class NoValue(Exception):
    pass


def enums_in(ddl: list[str]) -> dict[str, list[str]]:
    """`CREATE TYPE x AS ENUM (...)` labels, by unqualified lower-case type name."""
    out = {}
    for d in ddl:
        m = re.search(r"create\s+type\s+(?:[\w\"]+\.)?\"?([\w]+)\"?\s+as\s+enum\s*\((.*)\)", d, re.I | re.S)
        if m:
            out[m.group(1).lower()] = re.findall(r"'((?:[^']|'')*)'", m.group(2))
    return out


def value_for(raw_type: str, n: int, enums: dict[str, list[str]] | None = None) -> str:
    """Text of a distinct non-NULL value of `raw_type` for parameter `n`, as Postgres parses it.
    An enum has only as many values as labels, so for an enum distinctness is not guaranteed."""
    t = raw_type.strip().lower()
    bare = re.sub(r'^.*\.', '', t).strip('"')
    if enums and bare in enums and enums[bare]:
        labels = enums[bare]
        return labels[n % len(labels)].replace("''", "'")
    t = re.sub(r"^pg_catalog\.", "", t)
    if t.endswith("[]") or t.startswith("array"):
        raise NoValue(f"array type {raw_type}")
    m = re.match(r'^"?([a-z_ ]+?)"?\s*(\((\d+)(\s*,\s*\d+)?\))?(\s*with(out)? time zone)?$', t)
    if not m:
        raise NoValue(f"type {raw_type}")
    base, length = m.group(1).strip(), m.group(3)
    base = {"int": "integer", "int4": "integer", "int2": "smallint", "int8": "bigint",
            "serial": "integer", "serial4": "integer", "bigserial": "bigint", "serial8": "bigint",
            "smallserial": "smallint", "serial2": "smallint", "bool": "boolean",
            "float4": "real", "float8": "double precision", "float": "double precision",
            "decimal": "numeric", "character varying": "varchar", "character": "char",
            "bpchar": "char", "timestamptz": "timestamp", "timetz": "time"}.get(base, base)
    if base == "numeric" and length is not None:
        # numeric(p, s) holds p - s integer digits.
        scale = int(re.search(r",\s*(\d+)", m.group(2)).group(1)) if m.group(4) else 0
        if len(str(n)) > int(length) - scale:
            raise NoValue(f"{raw_type} cannot hold {n}")
    if base == "smallint" and n > 32767:
        raise NoValue(f"smallint cannot hold {n}")
    if base in ("integer", "smallint", "bigint", "numeric", "real", "double precision", "double", "oid"):
        return str(n)
    if base in ("text", "varchar", "char", "citext", "name"):
        s = _base36(n)
        if length is not None and len(s) > int(length):
            raise NoValue(f"{raw_type} is too short for distinct values")
        if length is None and base == "char":
            raise NoValue("char(1)")
        return s
    if base == "boolean":
        # Only two values exist, so distinctness is not guaranteed; the replay says so if it matters.
        return "true" if n % 2 else "false"
    if base == "uuid":
        return f"00000000-0000-4000-8000-{n:012d}"
    if base == "date":
        return str(datetime.date(2020, 1, 1) + datetime.timedelta(days=n))
    if base == "timestamp":
        return str(datetime.datetime(2020, 1, 1) + datetime.timedelta(seconds=n)) + ("+00" if "with time zone" in t or "timestamptz" in t else "")
    if base == "time":
        return str((datetime.datetime(2020, 1, 1) + datetime.timedelta(seconds=n)).time())
    if base == "interval":
        return f"{n} seconds"
    if base in ("json", "jsonb"):
        return json.dumps({"v": n})
    if base == "bytea":
        return "\\x" + format(n, "x").rjust(2 * ((len(format(n, "x")) + 1) // 2), "0")
    if base == "tsvector":
        return "w" + _base36(n)
    if base in ("inet", "cidr"):
        return f"10.{(n >> 16) & 255}.{(n >> 8) & 255}.{n & 255}"
    raise NoValue(f"type {raw_type}")


def sql_str(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def array_literal(items: list[str | None]) -> str:
    def el(v: str | None) -> str:
        if v is None:
            return "NULL"
        return '"' + v.replace("\\", "\\\\").replace('"', '\\"') + '"'
    return "{" + ",".join(el(v) for v in items) + "}"


# ---------------------------------------------------------------------------------------------------
# One pair.

def is_param(cell) -> bool:
    """A plan cell is a parameter number, `None` for `NULL`, or `{"gen": kind}` for a generated cell."""
    return isinstance(cell, int) and not isinstance(cell, bool)


# A parameter in a column that also holds generated cells is offset by this much, so that its
# canonical value cannot equal a value a sequence there generates (which starts at 1). The witness
# model takes the two to be distinct; without the offset Postgres could see them collide.
GEN_OFFSET = 20000


def bindings(plan: dict) -> list[str]:
    """The VALUES side's canonical binding, as literals."""
    types = {c["name"]: c["type"] for c in plan["columns"]}
    enums = enums_in(plan["ddl"])
    insert = plan["insert"]
    rows = plan["rows"]
    col_of: dict[int, int] = {}
    gen_cols = {j for r in rows for j, cell in enumerate(r) if isinstance(cell, dict)}
    for r in rows:
        for j, cell in enumerate(r):
            if is_param(cell):
                col_of.setdefault(cell, j)
    if not col_of:
        return []
    top = max(col_of)
    missing = [n for n in range(1, top + 1) if n not in col_of]
    if missing:
        raise NoValue(f"parameter ${missing[0]} is not used, so Postgres cannot type it")
    return [value_for(types[insert[col_of[n]]], n + (GEN_OFFSET if col_of[n] in gen_cols else 0), enums)
            for n in range(1, top + 1)]


def gather(plan: dict, scalars: list[str], g: list[list[str | None]] | None = None) -> list[str]:
    """The unnest side's gather binding, as literals: array `$j` is column `j` of the rows the VALUES
    side produced. A generated cell's entry is what it evaluated to, row `i`, column `j` of `g`."""
    rows = plan["rows"]
    arrays = []
    for j in range(len(plan["insert"])):
        col = []
        for i, r in enumerate(rows):
            cell = r[j]
            if cell is None:
                col.append(None)
            elif is_param(cell):
                col.append(scalars[cell - 1])
            else:
                col.append(g[i][j])
        arrays.append(array_literal(col))
    return arrays


def compared_columns(plan: dict) -> list[str]:
    """Columns whose values two runs of one statement must agree on: all of them, once `determinise`
    has fixed the generators. A default neither it nor this list recognises as deterministic is
    left out, and the record says so."""
    keep = []
    for c in plan["columns"]:
        d = c.get("default")
        if c["listed"] or d is None or deterministic_default(d):
            keep.append(c["name"])
    return keep


def deterministic_default(d: str) -> bool:
    if re.search(r"\bnextval\s*\(", d, re.I) or _CLOCK.search(d) or _DATE.search(d) or _UUID.search(d) or _RANDOM.search(d):
        rest = _RANDOM.sub("", _UUID.sub("", _DATE.sub("", _CLOCK.sub("", d))))
        return not re.search(r"\b(?!nextval\b|timezone\b|lpad\b)[a-z_]\w*\s*\(", rest, re.I)
    return bool(re.fullmatch(r"\s*\(?\s*(null|true|false|-?[\d.]+|'(?:[^']|'')*')(\s*::\s*[\w .\"\[\]()]+)*\s*\)?\s*", d, re.I))


_QUALIFIED = re.compile(
    r'\b(?:table|index\s+\S+\s+on|on|type|sequence|view|references|into)\s+(?:if\s+not\s+exists\s+)?(?:only\s+)?'
    r'("(?:[^"]|"")+"|[a-z_][\w$]*)\s*\.\s*(?:"(?:[^"]|"")+"|[a-z_][\w$]*)', re.I)


def schemas_in(texts: list[str]) -> list[str]:
    """Schemas a dump refers to by qualified name. Dumps often create objects in schemas they
    never create themselves."""
    seen = []
    for t in texts:
        for m in _QUALIFIED.finditer(t):
            sch = m.group(1)
            sch = sch[1:-1].replace('""', '"') if sch.startswith('"') else sch.lower()
            if sch != "pg_catalog" and sch not in seen:
                seen.append(sch)
    return seen


def unqualify(text: str, schemas: list[str]) -> str:
    """Drop `schema.` from every name qualified by one of `schemas`.

    sqleq resolves a table by the last part of its name, and captured dumps rely on that: a dump
    can create `t` while its indexes say `s1.t` and the pair says `s2.t`, which Postgres cannot
    make one table. Putting everything in `public` is the Postgres reading of sqleq's rule. The
    translator already refuses a last name that two declared tables share, and `localise` keeps the
    qualifiers on which the two statements' targets disagree, so this cannot merge two tables."""
    for sch in schemas:
        pat = '"' + re.escape(sch.replace('"', '""')) + '"' if not re.fullmatch(r"[a-z_][\w$]*", sch) else \
            r'(?:"' + re.escape(sch) + r'"|' + re.escape(sch) + r')'
        text = re.sub(r'(?<![\w".])' + pat + r'\s*\.\s*(?=["\w])', "", text, flags=re.I)
    return text


# Generators whose values would differ between two runs. The theorem claims the two sides agree
# under the *same* generator stream, so the replay gives both runs one: a clock is fixed (it is one
# value per statement anyway), and a random value is drawn from a sequence that lives, and is rolled
# back, with the run.
#
# Streams are split so that a draw on one side only cannot shift the other side's draws. With one
# shared stream, a generated cell that draws on the VALUES side but is supplied by the unnest side
# would shift every later draw on the VALUES side, and the two sides' other columns would differ
# for no reason the theorem is about. So each default in the DDL and each generator in a statement's
# tail draws from a sequence of its own, and every generator inside the VALUES clause from one
# shared sequence, in the order Postgres evaluates them, which the probe repeats. The stream's number
# is part of each value, so two streams never produce the same value. (Real random generators share
# no state, and never repeat.)
_CLOCK = re.compile(r"\b(?:now|statement_timestamp|transaction_timestamp)\s*\(\s*\)|\bcurrent_timestamp\b(?:\s*\(\s*\d*\s*\))?"
                    r"|\blocaltimestamp\b(?:\s*\(\s*\d*\s*\))?|\bclock_timestamp\s*\(\s*\)", re.I)
_DATE = re.compile(r"\bcurrent_date\b", re.I)
_UUID = re.compile(r"\b(?:public\.|pg_catalog\.)?(?:gen_random_uuid|uuid_generate_v4|uuid_generate_v7|uuid_generate_v1|uuid_generate_v1mc|uuidv4|uuidv7)\s*\(\s*\)", re.I)
_RANDOM = re.compile(r"\brandom\s*\(\s*\)", re.I)
FIXED_CLOCK = "'2020-06-01 12:00:00+00'::timestamptz"
# The variant nibble 9 sets these apart from the canonical parameter uuids (8, see `value_for`),
# and the next three hex digits are the stream's number.
SEQ_UUID = "(('00000000-0000-4000-9{n:03x}-' || lpad(nextval('{seq}')::text, 12, '0'))::uuid)"
SEQ_RANDOM = "(({n} * 1000000 + nextval('{seq}'))::double precision / 1e12)"


class Streams:
    """The generator sequences of one run, numbered from 1. `site(tag)` gives the stream for one
    place a generator is written; `shared(tag)` one stream for every generator under the tag."""

    def __init__(self):
        self.names: dict[str, int] = {}

    def _get(self, key: str) -> tuple[str, int]:
        n = self.names.setdefault(key, len(self.names) + 1)
        return f"sqleq_replay_{n}", n

    def site(self, tag: str):
        count = iter(range(1 << 30))
        return lambda: self._get(f"{tag}{next(count)}")

    def shared(self, tag: str):
        return lambda: self._get(tag)


def determinise(text: str, stream) -> str:
    text = _CLOCK.sub(FIXED_CLOCK, text)
    text = _DATE.sub("'2020-06-01'::date", text)

    def uuid(_):
        seq, n = stream()
        return SEQ_UUID.format(seq=seq, n=n)

    def rand(_):
        seq, n = stream()
        return SEQ_RANDOM.format(seq=seq, n=n)

    text = _UUID.sub(uuid, text)
    return _RANDOM.sub(rand, text)


def split_values(sql: str) -> tuple[str, str, str]:
    """`sql` as (before, `VALUES (…), (…)`, after), splitting at the top-level `VALUES` clause and
    respecting quotes and parentheses. `("", "", sql)` if there is none."""
    i, depth, n = 0, 0, len(sql)
    start = None
    while i < n:
        c = sql[i]
        if c in "'\"":
            j = i + 1
            while j < n and not (sql[j] == c and (j + 1 >= n or sql[j + 1] != c)):
                j += 2 if sql[j] == c else 1
            i = j + 1
            continue
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if start is not None and depth == 0:
                k = i + 1
                while k < n and sql[k].isspace():
                    k += 1
                if k >= n or sql[k] != ",":
                    return sql[:start], sql[start:i + 1], sql[i + 1:]
        elif start is None and depth == 0 and sql[i:i + 6].lower() == "values" \
                and (i == 0 or not (sql[i - 1].isalnum() or sql[i - 1] == "_")) \
                and (i + 6 >= n or not (sql[i + 6].isalnum() or sql[i + 6] == "_")):
            start = i
            i += 6
            continue
        i += 1
    return "", "", sql


_NAME_PART = r'(?:"(?:[^"]|"")+"|[a-z_][\w$]*)'
_INSERT_TARGET = re.compile(r'\binsert\s+into\s+(' + _NAME_PART + r'(?:\s*\.\s*' + _NAME_PART + r')*)', re.I)


def insert_target(sql: str) -> tuple[str, ...] | None:
    """The parts of the name an INSERT writes, each folded as Postgres folds it, with a leading
    `public` dropped (the replay puts every table there)."""
    m = _INSERT_TARGET.search(sql)
    if m is None:
        return None
    parts = tuple(p[1:-1].replace('""', '"') if p.startswith('"') else p.lower()
                  for p in re.findall(_NAME_PART, m.group(1), re.I))
    return parts[1:] if len(parts) > 1 and parts[0] == "public" else parts


def localise(plan: dict) -> dict:
    """The plan with every schema qualifier stripped (see `unqualify`) and every nondeterministic
    generator made deterministic (see `determinise`), in the DDL and in the statements alike.

    Except where the two statements' targets disagree: `a.events` on one side and `b.events` (or
    `events`) on the other are two tables, and stripping both would make them one, so the replay
    would confirm a pair that writes two different tables. Their qualifiers are kept in the
    statements, and Postgres, which has only the stripped DDL's tables, then sees the disagreement:
    a statement that does not prepare, an `inconclusive` rather than a `confirmed`.

    A default in the DDL and a generator in a statement's tail get a stream per site: the two sides'
    tails are the same text, so their sites line up. Every generator in the VALUES clause, on the
    VALUES side and in the probe's copy of it, draws from one stream."""
    schemas = schemas_in(plan["ddl"] + [plan["values_sql"], plan["unnest_sql"], plan["target"]])
    schemas += [x for x in ("public",) if x not in schemas]
    u = lambda t: unqualify(t, schemas)
    targets = insert_target(plan["values_sql"]), insert_target(plan["unnest_sql"])
    kept = set() if targets[0] == targets[1] else {q for t in targets for q in (t or ())[:-1]}
    u_stmt = lambda t: unqualify(t, [x for x in schemas if x not in kept])
    st = Streams()
    ddl = st.site("d")
    ddl_out = [determinise(u(d), ddl) for d in plan["ddl"]]

    def statement(sql: str) -> str:
        before, values, after = split_values(u_stmt(sql))
        tail = st.site("t")
        return determinise(before, tail) + determinise(values, st.shared("v")) + determinise(after, tail)

    out = {**plan, "ddl": ddl_out, "values_sql": statement(plan["values_sql"]),
           "unnest_sql": statement(plan["unnest_sql"]), "target": u_stmt(plan["target"])}
    if plan.get("values_clause") is not None:
        out["values_clause"] = determinise(u(plan["values_clause"]), st.shared("v"))
    out["sequences"] = [f"sqleq_replay_{n}" for n in sorted(st.names.values())]
    return out


def preamble(plan: dict) -> list[str]:
    """Open the run's transaction, create its generator sequences, and apply the DDL."""
    out = ["\\set ON_ERROR_STOP off", "\\set QUIET on", "\\pset format unaligned", "\\pset tuples_only on",
           "\\pset fieldsep '\\x1f'", "\\pset recordsep '\\x1d'", "\\pset null '\\x1e'", "BEGIN;"]
    out += [f"CREATE SEQUENCE {s};" for s in plan.get("sequences", [])]
    for i, d in enumerate(plan["ddl"]):
        # The terminator goes on its own line: a statement can end in a `--` comment.
        out += [f"SAVEPOINT d{i};", d.rstrip().rstrip(";") + "\n;",
                "\\if :ERROR", f"ROLLBACK TO SAVEPOINT d{i};", f"\\echo @@ddlfail {i} :LAST_ERROR_SQLSTATE",
                "\\else", f"RELEASE SAVEPOINT d{i};", "\\endif"]
    return out


def ident(c: str) -> str:
    return '"' + c.replace('"', '""') + '"'


def script(plan: dict, sql: str, args: list[list[str]], cols: list[str]) -> str:
    """One run: DDL, PREPARE, one EXECUTE per argument list, dump the table, roll back."""
    out = preamble(plan)
    out += ["\\echo @@prepare", f"PREPARE s AS {sql}\n;", "\\echo @@prepared :ERROR :LAST_ERROR_SQLSTATE"]
    for t, a in enumerate(args):
        call = f"EXECUTE s({', '.join(sql_str(x) for x in a)});" if a else "EXECUTE s;"
        out += [f"\\echo @@exec {t}", call, f"\\echo @@done {t} :ERROR :SQLSTATE :ROW_COUNT"]
    sel = ", ".join(ident(c) for c in cols) or "1"
    out += ["\\echo @@table", f"SELECT {sel} FROM {plan['target']};", "\\echo @@end", "ROLLBACK;"]
    return "\n".join(out) + "\n"


def probe_script(plan: dict, scalars: list[str], times: int) -> str:
    """Learn what the VALUES side's generated cells evaluate to: run its VALUES clause, `times`
    times, into a copy of the target with the target's defaults and identities, and read the rows
    back. The copy has no constraint but the NOT NULL of an identity, so the probe succeeds even
    where the VALUES side itself fails, and Postgres does the `DEFAULT` substitution, the coercion
    and the evaluation order itself. Rolled back, so the sequences it advances start afresh in the
    runs that follow."""
    cols = ", ".join(ident(c) for c in plan["insert"])
    out = preamble(plan)
    out += [f"CREATE TEMP TABLE sqleq_probe (LIKE {plan['target']} INCLUDING DEFAULTS INCLUDING IDENTITY);",
            "DO $$ DECLARE c text; BEGIN FOR c IN SELECT attname FROM pg_attribute "
            "WHERE attrelid = 'sqleq_probe'::regclass AND attnum > 0 AND attnotnull AND attidentity = '' "
            "LOOP EXECUTE format('ALTER TABLE sqleq_probe ALTER COLUMN %I DROP NOT NULL', c); END LOOP; END $$;",
            "\\echo @@prepare",
            f"PREPARE p AS INSERT INTO sqleq_probe ({cols}) {plan['values_clause']} RETURNING {cols}\n;",
            "\\echo @@prepared :ERROR :LAST_ERROR_SQLSTATE"]
    call = f"EXECUTE p({', '.join(sql_str(x) for x in scalars)});" if scalars else "EXECUTE p;"
    for t in range(times):
        out += [f"\\echo @@exec {t}", call, f"\\echo @@done {t} :ERROR :SQLSTATE :ROW_COUNT"]
    out += ["\\echo @@end", "ROLLBACK;"]
    return "\n".join(out) + "\n"


def fields(record: str) -> list[str | None]:
    """One output record's values; `\\x1e` is NULL."""
    return [None if v == "\x1e" else v for v in record.split("\x1f")]


def run(psql: list[str], text: str, timeout: int) -> str:
    p = subprocess.run(psql + ["-X", "-q", "-f", "-"], input=text, capture_output=True, text=True, timeout=timeout)
    return p.stdout


def parse(out: str, times: int) -> dict:
    """Read one run's output. Markers are `\\echo`ed lines starting `@@`; between them, a query's
    result is records separated by \\x1d, and a value may itself contain newlines, so records are
    never split on newlines."""
    res = {"ddl_failed": [], "prepared": None, "runs": [], "table": None}
    parts = re.split(r"(?m)^(@@[^\n]*)\n?", out)
    # parts = [before, marker, block, marker, block, ...]
    cur: list[str] | None = None
    in_table = False
    for i in range(1, len(parts), 2):
        marker, block = parts[i], parts[i + 1] if i + 1 < len(parts) else ""
        records = [r for r in block.rstrip("\n").split("\x1d") if r != ""] if block.strip("\n") else []
        f = marker.split()
        if f[0] == "@@ddlfail":
            res["ddl_failed"].append(f[1])
        elif f[0] == "@@prepared":
            res["prepared"] = f[1] == "false"
        elif f[0] == "@@exec":
            cur = records
        elif f[0] == "@@done":
            res["runs"].append({"ok": f[2] == "false", "sqlstate": f[3],
                                "rows": f[4] if len(f) > 4 else "", "returning": cur or []})
            cur = None
        elif f[0] == "@@table":
            res["table"] = sorted(records)
    return res


def outcome(r: dict) -> tuple:
    """What two runs must agree on. A failed run leaves an aborted transaction, so its table is not
    comparable and not compared."""
    runs = tuple((x["ok"], x["rows"], tuple(x["returning"])) if x["ok"] else (False,) for x in r["runs"])
    ok = all(x["ok"] for x in r["runs"])
    return runs + ((tuple(r["table"] or ()),) if ok else ())


CREDITED = ("proved-gather", "proved-gather-generated")


def probe(plan: dict, scalars: list[str], psql: list[str], timeout: int) -> list[list[list[str | None]]]:
    """What the generated cells evaluate to on each of two runs, as `g[run][row][column]`."""
    res = parse(run(psql, probe_script(plan, scalars, 2), timeout), 2)
    if res["prepared"] is not True or len(res["runs"]) != 2 or not all(r["ok"] for r in res["runs"]):
        raise NoValue("the probe for the generated values failed"
                      + (" (some DDL was rejected)" if res["ddl_failed"] else ""))
    out = []
    for r in res["runs"]:
        rows = [fields(x) for x in r["returning"]]
        if len(rows) != len(plan["rows"]) or any(len(x) != len(plan["insert"]) for x in rows):
            raise NoValue("the probe returned a different shape than the VALUES side")
        out.append(rows)
    return out


def probe_disagrees(plan: dict, cols: list[str], values_run: dict, g: list[list[list[str | None]]],
                    times: int) -> bool:
    """Triage for a pair with generated cells and no conflict clause: whether the VALUES side's own
    rows hold other generated values than the probe found. If so the probe, not the theorem, is what
    failed. Only a VALUES side that succeeded every time has rows to compare; anything else stays an
    alarm."""
    table = values_run["table"]
    if (table is None or not values_run["runs"] or not all(r["ok"] for r in values_run["runs"])
            or "on conflict" in plan["values_sql"].lower()):
        return False
    gen = sorted({j for r in plan["rows"] for j, c in enumerate(r) if isinstance(c, dict)})
    pos = [cols.index(plan["insert"][j]) if plan["insert"][j] in cols else None for j in gen]
    if None in pos:
        return False
    got = sorted(tuple(fields(rec)[p] for p in pos) for rec in table)
    want = sorted(tuple(g[t][i][j] for j in gen) for t in range(times) for i in range(len(plan["rows"])))
    return got != want


def replay(name: str, plan: dict, psql: list[str], timeout: int) -> dict:
    try:
        scalars = bindings(plan)
    except NoValue as e:
        return {"status": f"inconclusive: {e}"}
    cols = compared_columns(plan)
    rec: dict = {"lean": plan["verdict"]}
    plan = localise(plan)
    generated = plan.get("values_clause") is not None
    g = None
    if generated:
        try:
            g = probe(plan, scalars, psql, timeout)
        except NoValue as e:
            return {**rec, "status": f"inconclusive: {e}"}
        except subprocess.TimeoutExpired:
            return {**rec, "status": "inconclusive: psql timed out"}
    runs = {}
    for side, sql in (("values", plan["values_sql"]), ("unnest", plan["unnest_sql"])):
        for times in (1, 2):
            # The VALUES side runs under the same binding each time. The unnest side gathers that
            # binding, with each run's generated values: those values are different on the second run.
            if side == "values":
                args = [scalars] * times
            else:
                args = [gather(plan, scalars, g[t] if g else None) for t in range(times)]
            try:
                out = run(psql, script(plan, sql, args, cols), timeout)
            except subprocess.TimeoutExpired:
                return {**rec, "status": "inconclusive: psql timed out"}
            runs[(side, times)] = parse(out, times)
    first = runs[("values", 1)]
    if first["prepared"] is not True:
        failed = first["ddl_failed"]
        return {**rec, "status": "inconclusive: the VALUES side does not prepare" + (" (some DDL was rejected)" if failed else "")}
    if runs[("unnest", 1)]["prepared"] is not True:
        return {**rec, "status": "inconclusive: the unnest side does not prepare"}
    v1 = first["runs"][0] if first["runs"] else {"ok": False, "sqlstate": "?", "rows": ""}
    pg_ok = v1["ok"] and v1["rows"] not in ("", "0")
    rec["pg_values"] = {"ok": v1["ok"], "sqlstate": v1["sqlstate"], "rows": v1["rows"]}
    rec["ddl_failed"] = len(first["ddl_failed"])
    for times in (1, 2):
        if outcome(runs[("values", times)]) != outcome(runs[("unnest", times)]):
            rec["differs_on"] = "empty table" if times == 1 else "second run"
            rec["values_run"] = runs[("values", times)]["runs"]
            rec["unnest_run"] = runs[("unnest", times)]["runs"]
            if generated and probe_disagrees(plan, cols, runs[("values", times)], g, times):
                return {**rec, "status": "inconclusive: probe disagrees with the VALUES side"}
            return {**rec, "status": "ALARM-sides-differ"}
    model_ok = plan["verdict"] in CREDITED
    if model_ok != pg_ok:
        # If Postgres rejected DDL, it may be missing a constraint the model used, and then the
        # disagreement says nothing about the model.
        if first["ddl_failed"]:
            return {**rec, "status": "inconclusive: witness disagrees, but some DDL was rejected"}
        return {**rec, "status": "witness-disagrees"}
    return {**rec, "status": "confirmed" if model_ok else "confirmed-fails"}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--plan", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--psql", default=os.environ.get("PSQL", "psql"))
    ap.add_argument("--host")
    ap.add_argument("--port")
    ap.add_argument("--user")
    ap.add_argument("--dbname")
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--setup", action="store_true",
                    help="first install the contrib extensions dumps commonly use (uuid-ossp, pgcrypto, "
                         "citext, pg_trgm, hstore) into the database. The only write outside a rolled-back "
                         "transaction, so it is opt-in.")
    a = ap.parse_args()
    psql = [a.psql]
    for flag, v in (("-h", a.host), ("-p", a.port), ("-U", a.user), ("-d", a.dbname)):
        if v:
            psql += [flag, v]
    if a.setup:
        for ext in ("uuid-ossp", "pgcrypto", "citext", "pg_trgm", "hstore"):
            p = subprocess.run(psql + ["-X", "-q", "-c", f'CREATE EXTENSION IF NOT EXISTS "{ext}"'],
                               capture_output=True, text=True)
            print(f"extension {ext}: {'ok' if p.returncode == 0 else p.stderr.strip()}", file=sys.stderr)
    plans = json.load(open(a.plan))
    out: dict = {}
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        futs = {ex.submit(replay, n, p, psql, a.timeout): n for n, p in plans.items()}
        for f in cf.as_completed(futs):
            out[futs[f]] = f.result()
    json.dump(dict(sorted(out.items())), open(a.json, "w"), indent=1)
    counts: dict = {}
    for r in out.values():
        key = r["status"].split(":")[0]
        counts[key] = counts.get(key, 0) + 1
    print(json.dumps(counts, sort_keys=True))
    return 1 if counts.get("ALARM-sides-differ") else 0


if __name__ == "__main__":
    sys.exit(main())
