#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

"""Tests for sqleq_check's triviality classifier.

    python3 -m unittest discover -s tools -p 'test_*.py'

The classifier decides whether a pair is `x` against `x`, which is what the
`capability` number is computed from — so a mis-split here silently moves cases
between the two buckets and quietly inflates or deflates the headline. The
statement splitter is the part with real edge cases: a `;` inside a string
literal or a comment does not end a statement.

Standard library only, like the harness itself.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path

from sqleq_check import (REPO, SQLSOLVER_NO_PROOF, SQLSOLVER_PROVED, SQLSOLVER_PROVED_LITERAL,
                         SQLSOLVER_UNSUPPORTED, Case, SsDriver, _statements, build_parser,
                         resolve_axes, run_second_opinion, second_opinion, ss_slug,
                         triviality_from_ir, triviality_from_text)


class StatementSplitting(unittest.TestCase):
    def test_declarations_are_dropped(self):
        sql = (
            'create table "t" ("a" INTEGER);\n'
            "declare scalar function f(INTEGER) returns INTEGER;\n"
            'select "a" from "t";\nselect "a" from "t";'
        )
        self.assertEqual(_statements(sql),
                         ['select "a" from "t"', 'select "a" from "t"'])

    def test_whitespace_is_normalized(self):
        self.assertEqual(_statements("select   1\n  +\t2; select 1 + 2;"),
                         ["select 1 + 2", "select 1 + 2"])

    def test_semicolon_inside_a_string_literal_does_not_split(self):
        self.assertEqual(_statements("select ';' ; select ';';"),
                         ["select ';'", "select ';'"])

    def test_semicolon_inside_a_quoted_identifier_does_not_split(self):
        self.assertEqual(_statements('select "a;b" from "t"; select 1;'),
                         ['select "a;b" from "t"', "select 1"])

    def test_doubled_quote_is_an_escape_not_a_terminator(self):
        # If the doubled quote ended the literal, the `;` after it would split.
        self.assertEqual(_statements("select 'it''s; fine'; select 2;"),
                         ["select 'it''s; fine'", "select 2"])

    def test_line_comment_hides_a_semicolon(self):
        self.assertEqual(_statements("select 1 -- ; not a split\n; select 1;"),
                         ["select 1", "select 1"])

    def test_block_comment_hides_a_semicolon(self):
        self.assertEqual(_statements("select /* ; */ 1; select 1;"),
                         ["select 1", "select 1"])

    def test_unterminated_comment_swallows_the_rest(self):
        # Degenerate input; the point is that it terminates and yields a count
        # other than two, so the case is reported undetermined rather than
        # guessed at.
        self.assertIsNone(triviality_from_text("select 1; /* unterminated"))


class TrivialityFromText(unittest.TestCase):
    def test_identical_modulo_whitespace(self):
        self.assertIs(triviality_from_text(
            'create table "t" ("a" INTEGER);\nselect  "a"  from "t";\n'
            'select "a" from "t";'), True)

    def test_different(self):
        self.assertIs(triviality_from_text(
            'select "a" from "t"; select "b" from "t";'), False)

    def test_wrong_statement_count_is_undetermined(self):
        self.assertIsNone(triviality_from_text("select 1;"))
        self.assertIsNone(triviality_from_text("select 1; select 2; select 3;"))

    def test_the_text_test_is_blind_to_aliasing(self):
        # Both sides mean the same thing and lower to the same plan, but the
        # text differs. This is exactly why the IR test is preferred when there
        # is an IR to look at.
        self.assertIs(triviality_from_text(
            'select "a" as "x" from "t"; select "a" as "y" from "t";'), False)


class TrivialityFromIR(unittest.TestCase):
    def test_structurally_equal_plans(self):
        plan = {"queries": [{"scan": 0}, {"scan": 0}], "schemas": []}
        self.assertIs(triviality_from_ir(plan), True)

    def test_structurally_different_plans(self):
        plan = {"queries": [{"scan": 0}, {"scan": 1}], "schemas": []}
        self.assertIs(triviality_from_ir(plan), False)

    def test_key_order_does_not_matter(self):
        # Two dicts with the same entries are equal in Python regardless of
        # insertion order, which is the behaviour we want: the plan is a tree,
        # not a serialization.
        a = {"project": {"cols": [0, 1], "input": {"scan": 0}}}
        b = {"project": {"input": {"scan": 0}, "cols": [0, 1]}}
        self.assertIs(triviality_from_ir({"queries": [a, b]}), True)

    def test_list_order_does_matter(self):
        a = {"project": {"cols": [0, 1], "input": {"scan": 0}}}
        b = {"project": {"cols": [1, 0], "input": {"scan": 0}}}
        self.assertIs(triviality_from_ir({"queries": [a, b]}), False)

    def test_malformed_plans_are_undetermined(self):
        self.assertIsNone(triviality_from_ir({}))
        self.assertIsNone(triviality_from_ir({"queries": [{"scan": 0}]}))
        self.assertIsNone(triviality_from_ir({"queries": "not a list"}))


def _sqleq_solver_driver():
    """This repo's build of sqleq-solver, if there is one (it needs Z3 to build)."""
    for profile in ("release", "debug"):
        b = REPO / "target" / profile / "sqleq-solver"
        if b.is_file() and os.access(b, os.X_OK):
            return SsDriver("sqleq-solver", [str(b)], REPO, dict(os.environ), str(b))
    return None


@unittest.skipIf(_sqleq_solver_driver() is None, "sqleq-solver is not built")
class SecondOpinionThroughSqleqSolver(unittest.TestCase):
    """`run_second_opinion` end to end against sqleq-solver: the driver reads the jobs,
    writes IrDriver-shaped rows, and the harness buckets them exactly as it buckets the
    JVM's."""

    SCHEMA = [{"types": ["INTEGER"], "key": [], "nullable": [True]}]
    SCAN = {"scan": 0}

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="sqleq-ss-test-"))

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def job(self, name, ir, refusal=None):
        row = {"name": name, "ir": ir, "schema": ""}
        if refusal:
            row["refusal"] = refusal
        (self.dir / f"{ss_slug(name)}.job.jsonl").write_text(json.dumps(row) + "\n")
        return Case(name=name, path=name)

    def test_rows_are_bucketed_like_the_jvm_drivers(self):
        filtered = {"filter": {"source": self.SCAN,
                               "condition": {"operator": "=", "type": "BOOLEAN", "operand": [
                                   {"column": 0, "type": "INTEGER"},
                                   {"operator": "1", "operand": [], "type": "INTEGER"}]}}}
        sorted_scan = {"sort": {"source": self.SCAN,
                                "collation": [[0, "INTEGER", "ASCENDING NULLS LAST"]]}}
        cases = [
            self.job("same", {"schemas": self.SCHEMA, "queries": [self.SCAN, self.SCAN]}),
            # A bare ORDER BY is erased under bag semantics, so this is a real proof.
            self.job("proved", {"schemas": self.SCHEMA, "queries": [self.SCAN, sorted_scan]}),
            self.job("differs", {"schemas": self.SCHEMA, "queries": [self.SCAN, filtered]}),
            self.job("refused", None, refusal="unknown table t"),
            Case(name="no-job", path="no-job"),
        ]
        stats = run_second_opinion(cases, self.dir, _sqleq_solver_driver(), 10_000)
        got = {c.name: c.s_bucket for c in cases}
        self.assertEqual(got, {
            "same": SQLSOLVER_PROVED_LITERAL,
            "proved": SQLSOLVER_PROVED,
            "differs": SQLSOLVER_NO_PROOF,
            "refused": SQLSOLVER_UNSUPPORTED,
            "no-job": SQLSOLVER_UNSUPPORTED,
        })
        self.assertEqual((stats["rows"], stats["answered"], stats["halts"]), (4, 4, 0))
        self.assertEqual({c.name: c.s_note for c in cases}["refused"], "unknown table t")


class SecondOpinionOptions(unittest.TestCase):
    """`--sqleq-solver` asks sqleq-solver, `--sqlsolver-jvm` the JVM fork, and the spellings from
    before sqleq-solver was the default still mean what they meant."""

    def asks(self, *argv):
        return second_opinion(build_parser().parse_args(["x.sql", *argv]))

    def refused(self, *argv):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            self.asks(*argv)

    def test_nothing_is_asked_by_default(self):
        self.assertIsNone(self.asks())

    def test_each_switch_asks_its_prover(self):
        self.assertEqual(self.asks("--sqleq-solver"), "sqleq-solver")
        self.assertEqual(self.asks("--sqlsolver-jvm"), "jvm")

    def test_the_two_switches_together_are_refused(self):
        self.refused("--sqleq-solver", "--sqlsolver-jvm")

    def test_the_old_spellings_still_work(self):
        self.assertEqual(self.asks("--sqlsolver"), "sqleq-solver")
        self.assertEqual(self.asks("--sqlsolver", "--sqlsolver-impl", "rust"), "sqleq-solver")
        self.assertEqual(self.asks("--sqlsolver", "--sqlsolver-impl", "jvm"), "jvm")
        args = build_parser().parse_args(["x.sql", "--sqlsolver-bin", "b", "--sqlsolver-timeout", "5"])
        self.assertEqual((args.sqleq_solver_bin, args.sqleq_solver_timeout), ("b", 5))

    def test_an_unknown_implementation_is_refused(self):
        self.refused("--sqlsolver", "--sqlsolver-impl", "java")

    def test_the_switches_and_the_old_axis_name_choose_the_axis(self):
        axes = lambda *argv: resolve_axes(build_parser().parse_args(["x.sql", *argv]))
        self.assertEqual(axes("--sqleq-solver"), ["frontend", "qed", "sqleq-solver"])
        self.assertEqual(axes("--sqlsolver-jvm"), ["frontend", "qed", "sqlsolver-jvm"])
        self.assertEqual(axes("--axes", "sqlsolver-rust"), ["frontend", "sqleq-solver"])
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            axes("--axes", "sqleq-solver", "--sqlsolver-jvm")


class LeanAxis(unittest.TestCase):
    """`--lean` attaches sqleq-lean's verdicts to the right cases, by path."""

    def test_verdicts_attach_by_path_and_json_cases_are_skipped(self):
        import json
        import os
        import stat
        import tempfile
        from pathlib import Path

        from sqleq_check import Case, run_lean

        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp) / "sqleq-lean"
            # A stand-in: answers `proved-gather` for a path ending in a.sql, and says nothing
            # at all about any other, which the harness must report as `missing`.
            fake.write_text(
                "#!/usr/bin/env python3\n"
                "import json, sys\n"
                "a = sys.argv[1:]\n"
                "out = a[a.index('--json') + 1]\n"
                "paths = [x for x in a if x.endswith('.sql')]\n"
                "rec = {p: {'verdict': 'proved-gather', 'shape': 'row-major', 'ms': 3}\n"
                "       for p in paths if p.endswith('a.sql')}\n"
                "json.dump(rec, open(out, 'w'))\n"
            )
            fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
            a = Case(name="dir1/a.sql", path=os.path.join(tmp, "dir1", "a.sql"))
            b = Case(name="dir2/b.sql", path=os.path.join(tmp, "dir2", "b.sql"))
            plan = Case(name="p.json", path=os.path.join(tmp, "p.json"))
            stats = run_lean([a, b, plan], str(fake), jobs=2, timeout_s=10, keep_dir=None)
        self.assertEqual(stats["rows"], 2)
        self.assertEqual((a.l_verdict, a.l_shape, a.l_ms), ("proved-gather", "row-major", 3))
        self.assertEqual(b.l_verdict, "missing")
        self.assertIsNone(plan.l_verdict, "a pre-parsed .json plan has no pair file to read")


if __name__ == "__main__":
    unittest.main()
