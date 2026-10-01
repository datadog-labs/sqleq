#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

"""Re-run sqleq-lean's INSERT pairs on a real Postgres, as an independent check of the Lean axis.

`sqleq-lean --replay-plan plan.json` writes, for every pair it proved (`proved-gather`) or proved
without a witness (`no-witness`), the DDL, both statements, the VALUES rows and the target table's
column types and defaults. This script replays each pair on Postgres and asks two questions.

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
sequences start fresh each time. Columns the INSERT omits whose default is neither a sequence nor a
literal (`now()`, `gen_random_uuid()`, …) differ between any two runs, so they are left out of the
table comparison.

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
  cannot generate values for, …).
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

def bindings(plan: dict) -> tuple[list[str], list[str]]:
    """The VALUES side's canonical binding and the unnest side's gather binding, as literals."""
    types = {c["name"]: c["type"] for c in plan["columns"]}
    enums = enums_in(plan["ddl"])
    insert = plan["insert"]
    rows = plan["rows"]
    k = len(insert)
    col_of: dict[int, int] = {}
    for r in rows:
        for j, cell in enumerate(r):
            if cell is not None:
                col_of.setdefault(cell, j)
    if not col_of:
        scalars: list[str] = []
    else:
        top = max(col_of)
        missing = [n for n in range(1, top + 1) if n not in col_of]
        if missing:
            raise NoValue(f"parameter ${missing[0]} is not used, so Postgres cannot type it")
        scalars = [value_for(types[insert[col_of[n]]], n, enums) for n in range(1, top + 1)]
    arrays = []
    for j in range(k):
        arrays.append(array_literal([None if r[j] is None else scalars[r[j] - 1] for r in rows]))
    return scalars, arrays


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
    translator already refuses a last name that two tables share, so this cannot merge two
    tables."""
    for sch in schemas:
        pat = '"' + re.escape(sch.replace('"', '""')) + '"' if not re.fullmatch(r"[a-z_][\w$]*", sch) else \
            r'(?:"' + re.escape(sch) + r'"|' + re.escape(sch) + r')'
        text = re.sub(r'(?<![\w".])' + pat + r'\s*\.\s*(?=["\w])', "", text, flags=re.I)
    return text


# Generators whose values would differ between two runs. The theorem claims the two sides agree
# under the *same* generator stream, so the replay gives both runs one: a clock is fixed (it is one
# value per statement anyway), and a random value is drawn from a sequence that lives, and is rolled
# back, with the run.
_CLOCK = re.compile(r"\b(?:now|statement_timestamp|transaction_timestamp)\s*\(\s*\)|\bcurrent_timestamp\b(?:\s*\(\s*\d*\s*\))?"
                    r"|\blocaltimestamp\b(?:\s*\(\s*\d*\s*\))?|\bclock_timestamp\s*\(\s*\)", re.I)
_DATE = re.compile(r"\bcurrent_date\b", re.I)
_UUID = re.compile(r"\b(?:public\.)?(?:gen_random_uuid|uuid_generate_v4|uuid_generate_v1|uuid_generate_v1mc|uuidv4|uuidv7)\s*\(\s*\)", re.I)
_RANDOM = re.compile(r"\brandom\s*\(\s*\)", re.I)
FIXED_CLOCK = "'2020-06-01 12:00:00+00'::timestamptz"
SEQ_UUID = "(('00000000-0000-4000-8000-' || lpad(nextval('sqleq_replay_gen')::text, 12, '0'))::uuid)"
SEQ_RANDOM = "(nextval('sqleq_replay_gen')::double precision / 1e12)"


def determinise(ddl: str) -> str:
    ddl = _CLOCK.sub(FIXED_CLOCK, ddl)
    ddl = _DATE.sub("'2020-06-01'::date", ddl)
    ddl = _UUID.sub(SEQ_UUID, ddl)
    return _RANDOM.sub(SEQ_RANDOM, ddl)


def localise(plan: dict) -> dict:
    """The plan with every schema qualifier stripped (see `unqualify`) and every nondeterministic
    default made deterministic (see `determinise`)."""
    schemas = schemas_in(plan["ddl"] + [plan["values_sql"], plan["unnest_sql"], plan["target"]])
    schemas += [x for x in ("public",) if x not in schemas]
    u = lambda t: unqualify(t, schemas)
    return {**plan, "ddl": [determinise(u(d)) for d in plan["ddl"]], "values_sql": u(plan["values_sql"]),
            "unnest_sql": u(plan["unnest_sql"]), "target": u(plan["target"])}


def script(plan: dict, sql: str, args: list[str], times: int, cols: list[str]) -> str:
    """One run: DDL, PREPARE, EXECUTE `times` times, dump the table, roll back."""
    out = ["\\set ON_ERROR_STOP off", "\\set QUIET on", "\\pset format unaligned", "\\pset tuples_only on",
           "\\pset fieldsep '\\x1f'", "\\pset recordsep '\\x1d'", "\\pset null '\\x1e'", "BEGIN;",
           "CREATE SEQUENCE sqleq_replay_gen;"]
    for i, d in enumerate(plan["ddl"]):
        # The terminator goes on its own line: a statement can end in a `--` comment.
        out += [f"SAVEPOINT d{i};", d.rstrip().rstrip(";") + "\n;",
                "\\if :ERROR", f"ROLLBACK TO SAVEPOINT d{i};", f"\\echo @@ddlfail {i} :LAST_ERROR_SQLSTATE",
                "\\else", f"RELEASE SAVEPOINT d{i};", "\\endif"]
    out += ["\\echo @@prepare", f"PREPARE s AS {sql}\n;", "\\echo @@prepared :ERROR :LAST_ERROR_SQLSTATE"]
    call = f"EXECUTE s({', '.join(sql_str(a) for a in args)});" if args else "EXECUTE s;"
    for t in range(times):
        out += [f"\\echo @@exec {t}", call, f"\\echo @@done {t} :ERROR :SQLSTATE :ROW_COUNT"]
    sel = ", ".join('"' + c.replace('"', '""') + '"' for c in cols) or "1"
    out += ["\\echo @@table", f"SELECT {sel} FROM {plan['target']};", "\\echo @@end", "ROLLBACK;"]
    return "\n".join(out) + "\n"


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


def replay(name: str, plan: dict, psql: list[str], timeout: int) -> dict:
    try:
        scalars, arrays = bindings(plan)
    except NoValue as e:
        return {"status": f"inconclusive: {e}"}
    cols = compared_columns(plan)
    rec: dict = {"lean": plan["verdict"]}
    plan = localise(plan)
    runs = {}
    for side, sql, args in (("values", plan["values_sql"], scalars), ("unnest", plan["unnest_sql"], arrays)):
        for times in (1, 2):
            try:
                out = run(psql, script(plan, sql, args, times, cols), timeout)
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
            return {**rec, "status": "ALARM-sides-differ"}
    model_ok = plan["verdict"] == "proved-gather"
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
