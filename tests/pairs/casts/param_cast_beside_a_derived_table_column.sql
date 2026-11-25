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
-- origin: #302's review, pair (a): a comparison with a column of a derived table states no type the frontend
--   reads, and for a type inference has a class for such a use left the cast to inference, which dropped it
-- witness: t = {(1, '2020-01-01 10:00:00')}, $1 = '2020-01-01 10:00:00': in A $1 is a date, and A compares ts with
--   midnight and returns no row. In B $1 is a timestamp, and B returns 1
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT q.id FROM (SELECT id, ts FROM t) AS q WHERE q.ts = $1::date;
SELECT q.id FROM (SELECT id, ts FROM t) AS q WHERE q.ts = $1;
