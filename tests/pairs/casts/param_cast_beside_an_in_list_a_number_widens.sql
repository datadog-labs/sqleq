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
-- origin: #302's review, pair (f): the constant 2.5 makes the list's common type numeric, so $1 is a numeric in
--   both statements and $1::int rounds it
-- witness: t = {(1, a 7, b 1)}, $1 = '1.4': A compares b with 1.4::int, which is 1, and returns 1. B compares it
--   with 1.4 and returns no row
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE a NOT IN ($1, 2.5) AND b = $1::int;
SELECT id FROM t WHERE a NOT IN ($1, 2.5) AND b = $1;
