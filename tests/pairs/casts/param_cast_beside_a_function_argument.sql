-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #302's review, pair (d): an argument of a function states no type the frontend reads. abs makes $1 a
--   double precision in both statements, so $1::int rounds it
-- witness: t = {(1, a 0)}, $1 = '0.4': A compares a with 0.4::int, which is 0, and returns 1. B compares it with
--   0.4 and returns no row
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE abs($1) < 1 AND a = $1::int;
SELECT id FROM t WHERE abs($1) < 1 AND a = $1;
