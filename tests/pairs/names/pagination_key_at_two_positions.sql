-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #74: strip_identical_pagination dropped an identical ORDER BY ... LIMIT once each
--   side's key was determined by its own projection, without checking it was the same output
--   column: A's key a is its second column (t.b), B's its first (t.a)
-- witness: t = {(1, 1, 2), (2, 2, 1)}: A returns (2, 1), B returns (1, 2)

create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT a AS b, b AS a FROM t ORDER BY a LIMIT 1;
SELECT a, b FROM t ORDER BY a LIMIT 1;
