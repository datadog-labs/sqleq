#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

"""Tests for the pinned-pair suite: `sqleq_suite.py` and `sqleq_check.py --expect pinned`.

    python3 -m unittest discover -s tools -p 'test_*.py'

The load-bearing tests are the controls through `main()`: a fake prover that proves a
non-equivalent pair must fail the run, with and without `--bless`, and must leave the file
alone. A suite whose invariant check had stopped firing would pass every other test here.

Standard library only, like the harness itself.
"""

from __future__ import annotations

import contextlib
import io
import os
import re
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import sqleq_suite as s
from sqleq_check import REPO, classify_refusal, fuzz_one, main

LICENCE = """-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.
"""
SQL = 'create table "t" ("a" INTEGER);\nSELECT "a" FROM "t";\nSELECT "a" FROM "t" WHERE "a" = 1;\n'


def pair(*directives: str, sql: str = SQL) -> str:
    return LICENCE + "\n" + "".join(d + "\n" for d in directives) + "\n" + sql


NEQ_OK = ("-- truth: not-equivalent", "-- origin: a test", "-- witness: t = {(0)}")
EQ_OK = ("-- truth: equivalent", "-- origin: a test", "-- argument: because")


class Lint(unittest.TestCase):
    def errors(self, *directives, sql=SQL):
        return s.lint(s.parse_header(pair(*directives, sql=sql)))

    def assertLints(self, needle, *directives, sql=SQL):
        errs = self.errors(*directives, sql=sql)
        self.assertTrue(any(needle in e for e in errs), f"no {needle!r} in {errs}")

    def test_a_complete_header_is_clean(self):
        self.assertEqual(self.errors(*NEQ_OK, "-- expect qed: no-proof"), [])
        self.assertEqual(self.errors(*EQ_OK, "-- expect fuzz: no-counterexample"), [])

    def test_the_licence_and_prose_are_not_directives(self):
        h = s.parse_header(pair(*NEQ_OK, "-- A sentence: with a colon in it."))
        self.assertEqual(h.errors, [])
        self.assertEqual(set(h.lines), {"truth", "origin", "witness"})

    def test_rows(self):
        rows = [
            ("no `truth:` line", ("-- origin: x", "-- witness: w")),
            ("is not one of", ("-- truth: maybe", "-- origin: x", "-- argument: a")),
            ("no `origin:` line", ("-- truth: equivalent", "-- argument: a")),
            ("unknown axis `fuz`", NEQ_OK + ("-- expect fuz: counterexample",)),
            ("unknown directive `note:`", NEQ_OK + ("-- note: hello",)),
            ("duplicate `expect qed:`", NEQ_OK + ("-- expect qed: no-proof",
                                                  "-- expect qed: no-proof")),
            ("`proven` is not one of", NEQ_OK + ("-- expect qed: proven",)),
            ("never pinnable", NEQ_OK + ("-- expect qed: timeout",)),
            ("takes one word", NEQ_OK + ("-- expect qed: no-proof extra",)),
            ("does not contradict", NEQ_OK + ("-- expect qed: no-proof !known-unsound",)),
            ("does not contradict", EQ_OK + ("-- expect qed: proved !known-unsound",)),
            ("needs `expect fuzz: counterexample` or a `witness:`",
             ("-- truth: not-equivalent", "-- origin: x", "-- expect fuzz: no-counterexample")),
            ("needs a prover's `proved` pin or an `argument:`",
             ("-- truth: equivalent", "-- origin: x", "-- expect qed: no-proof")),
            ("needs a prover's `proved` pin or an `argument:`",
             # A proof that is itself marked unsound is no evidence for anything.
             ("-- truth: equivalent", "-- origin: x", "-- expect fuzz: counterexample")),
            ("`catalog: guessed` is not one of", EQ_OK + ("-- catalog: guessed",)),
            ("`binding: sideways` is not one of", EQ_OK + ("-- binding: sideways",)),
            # A contradicting pin can only have been written by hand; it is caught without the axis.
            ("contradicts `truth: not-equivalent`", NEQ_OK + ("-- expect qed: proved",)),
            ("contradicts `truth: equivalent`", EQ_OK + ("-- expect fuzz: counterexample",)),
            ("contradicts `truth: not-equivalent`",
             NEQ_OK + ("-- binding: gather", "-- expect lean: no-witness")),
            # Under the gather rule only Lean's answers are evidence, and `no-witness` is not.
            ("a non-equivalent pair needs a `witness:`",
             ("-- truth: not-equivalent", "-- binding: gather", "-- origin: x",
              "-- expect fuzz: counterexample")),
            ("needs `expect lean: proved-gather` or an `argument:`",
             ("-- truth: equivalent", "-- binding: gather", "-- origin: x",
              "-- expect qed: proved")),
            ("needs `expect lean: proved-gather` or an `argument:`",
             ("-- truth: equivalent", "-- binding: gather", "-- origin: x",
              "-- expect lean: no-witness")),
        ]
        for needle, directives in rows:
            with self.subTest(needle=needle, directives=directives):
                self.assertLints(needle, *directives)

    def test_evidence_from_an_axis_replaces_a_written_one(self):
        self.assertEqual(self.errors("-- truth: not-equivalent", "-- origin: x",
                                     "-- expect fuzz: counterexample"), [])
        self.assertEqual(self.errors("-- truth: equivalent", "-- origin: x",
                                     "-- expect qed: proved"), [])
        self.assertEqual(self.errors("-- truth: equivalent", "-- binding: gather", "-- origin: x",
                                     "-- expect lean: proved-gather"), [])

    def test_an_axis_under_the_other_binding_contradicts_nothing(self):
        # The other axes refuse a gather pair, and Lean has nothing to say about an index one.
        self.assertEqual(self.errors("-- truth: equivalent", "-- binding: gather", "-- origin: x",
                                     "-- argument: a", "-- expect fuzz: counterexample"), [])
        self.assertEqual(self.errors(*NEQ_OK, "-- expect lean: proved-gather"), [])

    def test_a_pin_below_the_sql_is_an_error_not_a_comment(self):
        self.assertLints("below the first SQL line", *NEQ_OK,
                         sql=SQL + "-- expect qed: no-proof\n")

    def test_a_marker_on_a_real_contradiction_is_clean(self):
        for directives in (NEQ_OK + ("-- expect qed: proved !known-unsound",),
                           NEQ_OK + ("-- expect frontend: emit-reflexive !known-unsound",),
                           EQ_OK + ("-- expect fuzz: counterexample !known-unsound",)):
            with self.subTest(directives=directives):
                self.assertEqual(self.errors(*directives), [])


class Judge(unittest.TestCase):
    def judge(self, observed, *directives):
        h = s.parse_header(pair(*directives))
        return {j.axis: j.state for j in s.judge(h, observed)}

    def test_rows(self):
        neq, eq = NEQ_OK, EQ_OK
        rows = [
            ("holds", neq + ("-- expect qed: no-proof",), {"qed": ("no-proof", "")}, s.OK),
            # The ratchet: an improvement is a movement too, and fails until blessed.
            ("improvement", eq + ("-- expect qed: no-proof",), {"qed": ("proved", "")}, s.CHANGED),
            ("regression", eq + ("-- expect qed: proved",), {"qed": ("no-proof", "")}, s.CHANGED),
            ("unpinned", neq, {"qed": ("no-proof", "")}, s.UNPINNED),
            ("false proof", neq, {"qed": ("proved", "")}, s.INVARIANT),
            ("false proof over a pin", neq + ("-- expect qed: no-proof",),
             {"qed": ("proved-literal", "")}, s.INVARIANT),
            ("lowered alike", neq, {"frontend": ("emit-reflexive", "")}, s.INVARIANT),
            ("false refutation", eq, {"fuzz": ("counterexample", "")}, s.INVARIANT),
            ("known", neq + ("-- expect qed: proved !known-unsound",), {"qed": ("proved", "")},
             s.KNOWN),
            ("known, another word", neq + ("-- expect qed: proved !known-unsound",),
             {"qed": ("proved-literal", "")}, s.CHANGED),
            ("fixed", neq + ("-- expect qed: proved !known-unsound",), {"qed": ("no-proof", "")},
             s.STALE),
            ("timeout", neq + ("-- expect qed: no-proof",), {"qed": ("timeout", "")},
             s.UNANSWERED),
            ("missing", neq, {"sqlsolver-jvm": ("missing", "")}, s.UNANSWERED),
            ("lean false proof", neq + ("-- binding: gather",), {"lean": ("proved-gather", "")},
             s.INVARIANT),
            ("lean vacuous false proof", neq + ("-- binding: gather",),
             {"lean": ("no-witness", "")}, s.INVARIANT),
            ("lean on an index pair", neq, {"lean": ("proved-gather", "")}, s.UNPINNED),
            ("prover on a gather pair", neq + ("-- binding: gather",), {"qed": ("proved", "")},
             s.UNPINNED),
            ("fuzz on a gather pair", eq + ("-- binding: gather",),
             {"fuzz": ("counterexample", "")}, s.UNPINNED),
        ]
        for label, directives, observed, want in rows:
            with self.subTest(label):
                self.assertEqual(self.judge(observed, *directives), {next(iter(observed)): want})

    def test_an_axis_that_did_not_run_is_not_judged(self):
        got = self.judge({"fuzz": ("counterexample", "")}, *NEQ_OK, "-- expect qed: proved "
                         "!known-unsound")
        self.assertEqual(got, {"fuzz": s.UNPINNED})


class Bless(unittest.TestCase):
    def bless(self, text, observed):
        h = s.parse_header(text)
        return s.bless_text(text, h, s.judge(h, observed))

    def test_only_expect_lines_change(self):
        text = pair(*NEQ_OK, "-- expect qed: proved !known-unsound", "-- expect fuzz: error")
        out = self.bless(text, {"qed": ("no-proof", ""), "fuzz": ("counterexample", "")})
        old, new = text.splitlines(), out.splitlines()
        self.assertEqual(len(old), len(new))
        moved = [(a, b) for a, b in zip(old, new) if a != b]
        self.assertEqual(moved, [("-- expect qed: proved !known-unsound", "-- expect qed: no-proof"),
                                 ("-- expect fuzz: error", "-- expect fuzz: counterexample")])

    def test_missing_lines_go_in_canonical_order_whatever_ran_first(self):
        observed = {"frontend": ("emit", ""), "fuzz": ("counterexample", ""),
                    "qed": ("no-proof", ""), "sqlsolver-rust": ("no-proof", "")}
        want = None
        for order in (["sqlsolver-rust", "qed"], ["qed", "fuzz", "frontend", "sqlsolver-rust"],
                      ["frontend"]):
            text = pair(*NEQ_OK)
            for axis in order + list(observed):
                text = self.bless(text, {axis: observed[axis]})
            want = want or text
            with self.subTest(order=order):
                self.assertEqual(text, want)
        axes = [ln.split(":")[0] for ln in want.splitlines() if ln.startswith("-- expect ")]
        self.assertEqual(axes, ["-- expect frontend", "-- expect fuzz", "-- expect qed",
                                "-- expect sqlsolver-rust"])
        self.assertLess(want.index("-- truth:"), want.index("-- expect frontend"))

    def test_a_second_bless_is_a_no_op(self):
        observed = {"frontend": ("emit", ""), "fuzz": ("counterexample", "")}
        once = self.bless(pair(*NEQ_OK), observed)
        self.assertNotEqual(once, pair(*NEQ_OK))
        self.assertEqual(self.bless(once, observed), once)

    def test_an_invariant_is_never_pinned_and_a_marker_never_added(self):
        text = pair(*NEQ_OK, "-- expect qed: no-proof")
        self.assertEqual(self.bless(text, {"qed": ("proved", "")}), text)
        self.assertEqual(self.bless(pair(*NEQ_OK), {"qed": ("proved", "")}), pair(*NEQ_OK))

    def test_no_answer_is_never_pinned(self):
        text = pair(*NEQ_OK, "-- expect qed: no-proof")
        self.assertEqual(self.bless(text, {"qed": ("timeout", "")}), text)

    def test_a_marker_survives_while_its_contradiction_does(self):
        text = pair(*NEQ_OK, "-- expect qed: proved !known-unsound")
        out = self.bless(text, {"qed": ("proved-literal", "")})
        self.assertIn("-- expect qed: proved-literal !known-unsound\n", out)

    def test_line_endings_and_the_final_newline_are_kept(self):
        crlf = pair(*NEQ_OK).replace("\n", "\r\n")
        out = self.bless(crlf, {"qed": ("no-proof", "")})
        self.assertIn("-- expect qed: no-proof\r\n", out)
        self.assertNotIn("\r\n", out.replace("\r\n", ""))  # no bare \n crept in
        bare = pair(*NEQ_OK).rstrip("\n")
        self.assertFalse(self.bless(bare, {"qed": ("no-proof", "")}).endswith("\n"))

    def test_write_if_changed_does_not_touch_an_unchanged_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "a.sql"
            p.write_text("x\n")
            os.utime(p, (0, 0))
            self.assertFalse(s.write_if_changed(p, "x\n"))
            self.assertEqual(p.stat().st_mtime, 0)
            self.assertTrue(s.write_if_changed(p, "y\n"))
            self.assertEqual(p.read_text(), "y\n")


def _exe(path: Path, body: str) -> str:
    path.write_text("#!/usr/bin/env python3\n" + body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC)
    return str(path)


# Stand-ins for the three binaries. The frontend lowers every pair to two different plans and
# records its arguments; the solver answers $FAKE_SS for every job; fuzz prints $FAKE_FUZZ.
FAKE_FRONTEND = """import json, os, sys
a = sys.argv[1:]
with open(os.environ["FAKE_ARGV"], "a") as f:
    f.write(json.dumps(a) + "\\n")
if "--sqlsolver" in a:
    name, out = a[a.index("--name") + 1], a[a.index("-o") + 1]
    json.dump({"name": name, "ir": {"queries": [1, 2]}, "schema": ""}, open(out, "w"))
    sys.exit(0)
pos = [x for x in a if not x.startswith("--")]
json.dump({"schemas": [], "queries": [{"scan": 0}, {"scan": 1}]}, open(pos[1], "w"))
"""
FAKE_SOLVER = """import json, os, sys
jobs, out = sys.argv[1], sys.argv[2]
with open(out, "a") as f:
    for line in open(jobs):
        f.write(json.dumps({"name": json.loads(line)["name"], "verdict": os.environ["FAKE_SS"],
                            "ms": 1}) + "\\n")
"""
FAKE_FUZZ = """import os
print(os.environ["FAKE_FUZZ"])
"""


class ThroughMain(unittest.TestCase):
    """`main()` end to end, over fake binaries."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="sqleq-suite-test-"))
        self.frontend = _exe(self.tmp / "fe", FAKE_FRONTEND)
        self.solver = _exe(self.tmp / "ss", FAKE_SOLVER)
        self.fuzz = _exe(self.tmp / "fz", FAKE_FUZZ)
        self.argv_log = self.tmp / "argv.jsonl"
        self.env = mock.patch.dict(os.environ, {"FAKE_ARGV": str(self.argv_log), "FAKE_SS": "NEQ",
                                                "FAKE_FUZZ": "NO-COUNTEREXAMPLE"})
        self.env.start()
        self.case = self.tmp / "case.sql"

    def tearDown(self):
        self.env.stop()
        import shutil
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run_main(self, *extra, axes="frontend,sqlsolver-rust", paths=None):
        argv = ["--expect", "pinned", "--axes", axes, "--frontend", self.frontend,
                "--sqlsolver-bin", self.solver, "--fuzz-bin", self.fuzz, "-j", "1",
                *extra, *(paths or [str(self.case)])]
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = main(argv)
        self.out, self.err = out.getvalue(), err.getvalue()
        return rc

    def test_a_false_proof_fails_and_bless_will_not_pin_it(self):
        os.environ["FAKE_SS"] = "EQ"
        text = pair(*NEQ_OK, "-- expect frontend: emit")
        self.case.write_text(text)
        self.assertEqual(self.run_main(), 1)
        self.assertIn("‼ proved", self.out)
        self.assertEqual(self.run_main("--bless"), 1, "a bless run that leaves an invariant fails")
        self.assertEqual(self.case.read_text(), text, "bless must not touch an invariant")

        # A person marks it known: the run passes while the bug reproduces …
        self.case.write_text(pair(*NEQ_OK, "-- expect frontend: emit",
                                  "-- expect sqlsolver-rust: proved !known-unsound"))
        self.assertEqual(self.run_main(), 0)
        # … and fails the run that fixes it, until bless drops the marker.
        os.environ["FAKE_SS"] = "NEQ"
        self.assertEqual(self.run_main(), 1)
        self.assertIn("no longer reproduces", self.out)
        self.assertEqual(self.run_main("--bless"), 0)
        self.assertIn("-- expect sqlsolver-rust: no-proof\n", self.case.read_text())
        self.assertNotIn("!known-unsound", self.case.read_text())

    def test_a_false_refutation_fails(self):
        os.environ["FAKE_FUZZ"] = "NOT-EQUIVALENT"
        self.case.write_text(pair(*EQ_OK))
        self.assertEqual(self.run_main("--bless", axes="fuzz"), 1)
        self.assertNotIn("-- expect fuzz:", self.case.read_text())

    def test_bless_round_trip(self):
        self.case.write_text(pair(*NEQ_OK))
        self.assertEqual(self.run_main(axes="frontend,fuzz,sqlsolver-rust"), 1)  # unpinned
        self.assertEqual(self.run_main("--bless", axes="frontend,fuzz,sqlsolver-rust"), 0)
        blessed = self.case.read_text()
        for line in ("-- expect frontend: emit\n", "-- expect fuzz: no-counterexample\n",
                     "-- expect sqlsolver-rust: no-proof\n"):
            self.assertIn(line, blessed)
        self.assertEqual(self.run_main(axes="frontend,fuzz,sqlsolver-rust"), 0)
        self.assertEqual(self.run_main("--bless", axes="frontend,fuzz,sqlsolver-rust"), 0)
        self.assertEqual(self.case.read_text(), blessed)
        self.assertIn("blessed 0 file(s)", self.out)

    def test_an_improvement_fails_until_blessed(self):
        os.environ["FAKE_SS"] = "EQ"
        self.case.write_text(pair(*EQ_OK, "-- expect frontend: emit",
                                  "-- expect sqlsolver-rust: no-proof"))
        self.assertEqual(self.run_main(), 1)
        self.assertIn("no-proof→proved", self.out)

    def test_a_lint_error_fails_and_bless_skips_the_file(self):
        text = pair("-- truth: not-equivalent", "-- origin: x")
        self.case.write_text(text)
        self.assertEqual(self.run_main("--bless"), 1)
        self.assertEqual(self.case.read_text(), text)

    def test_relative_binary_paths_work(self):
        """Each case runs in its own working directory, so a relative `--frontend` used to
        name nothing there. CI passes `target/debug/...`, which is how this was found."""
        self.case.write_text(pair(*NEQ_OK, "-- expect frontend: emit",
                                  "-- expect fuzz: no-counterexample",
                                  "-- expect sqlsolver-rust: no-proof"))
        cwd = os.getcwd()
        os.chdir(self.tmp)
        try:
            self.frontend, self.solver, self.fuzz = "fe", "ss", "fz"
            self.assertEqual(self.run_main(axes="frontend,fuzz,sqlsolver-rust",
                                           paths=["case.sql"]), 0, self.out + self.err)
        finally:
            os.chdir(cwd)

    def test_the_catalog_header_reaches_the_frontend(self):
        self.case.write_text(pair(*NEQ_OK, "-- catalog: inferred-seeded"))
        self.run_main()
        self.assertIn('"--infer-seeded"', self.argv_log.read_text().splitlines()[0])

    def test_fuzz_alone_needs_no_frontend(self):
        self.case.write_text(pair(*NEQ_OK, "-- expect fuzz: no-counterexample"))
        self.assertEqual(self.run_main(axes="fuzz"), 0)
        self.assertFalse(self.argv_log.exists())

    def test_setup_errors_exit_2(self):
        """A missing tool or a bad flag is exit 2, never 1: it is not a case that failed."""
        self.case.write_text(pair(*NEQ_OK))
        plan = self.tmp / "plan.json"
        plan.write_text("{}")
        rows = [
            (["--expect", "pinned", "--axes", "fuzz", "--fuzz-bin", str(self.tmp / "nope")],
             self.case),
            (["--expect", "pinned", "--axes", "frontend,qd"], self.case),
            (["--expect", "pinned", "--axes", "sqlsolver-rust,sqlsolver-jvm"], self.case),
            (["--expect", "equivalent", "--axes", "frontend"], self.case),
            (["--expect", "report-only", "--bless"], self.case),
            (["--expect", "pinned", "--axes", "frontend"], plan),
        ]
        for argv, path in rows:
            with self.subTest(argv=argv):
                with contextlib.redirect_stdout(io.StringIO()), \
                        contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(main(argv + ["--frontend", self.frontend, str(path)]), 2)


class Pieces(unittest.TestCase):
    def test_classify_refusal_knows_all_four_kinds(self):
        self.assertEqual(classify_refusal("PARSE ERROR: x")[0], "parse")
        self.assertEqual(classify_refusal("unsupported: x")[0], "unsupported")
        self.assertEqual(classify_refusal("parameter-misaligned: arity")[0],
                         "parameter-misaligned")
        self.assertEqual(classify_refusal("unresolved column x")[0], "schema")

    def test_fuzz_labels_map_to_their_kind(self):
        with tempfile.TemporaryDirectory() as tmp:
            case = Path(tmp) / "c.sql"
            case.write_text("")
            rows = [
                ('print("NOT-EQUIVALENT"); print("counterexample: t=[(0)]")',
                 ("counterexample", "t=[(0)]")),
                ('print("NO-COUNTEREXAMPLE")', ("no-counterexample", "")),
                ('print("PARAM-MISALIGNED:$1 vs $2")', ("param-misaligned", "$1 vs $2")),
                ('print("ERROR:Binder Error")', ("error", "Binder Error")),
                ('import sys; sys.stderr.write("boom\\n"); sys.exit(1)', ("error", "boom")),
            ]
            for body, want in rows:
                with self.subTest(body=body):
                    fake = _exe(Path(tmp) / "fz", body + "\n")
                    self.assertEqual(fuzz_one(fake, str(case), 10)[:2], want)


class TheRealCases(unittest.TestCase):
    """Hygiene over the committed cases: each one lints, carries the licence, and names nothing
    that belongs to a corpus or a machine."""

    FILES = sorted((REPO / "tests" / "pairs").rglob("*.sql")) + sorted((REPO / "examples").glob("*.sql"))

    def test_there_are_cases(self):
        self.assertGreaterEqual(len(self.FILES), 10)

    def test_each_case_lints_clean(self):
        for f in self.FILES:
            with self.subTest(case=str(f.relative_to(REPO))):
                self.assertEqual(s.lint(s.parse_header(f.read_text())), [])

    def test_each_case_carries_the_licence(self):
        for f in self.FILES:
            with self.subTest(case=str(f.relative_to(REPO))):
                self.assertIn("Apache License Version 2.0", "".join(f.read_text().splitlines(True)[:6]))

    def test_no_case_names_a_corpus_row_or_a_machine(self):
        bad = re.compile(r"\bpair\d{2,}\b|/home/|/Users/")
        for f in self.FILES:
            with self.subTest(case=str(f.relative_to(REPO))):
                self.assertIsNone(bad.search(f.read_text()))


if __name__ == "__main__":
    unittest.main()
