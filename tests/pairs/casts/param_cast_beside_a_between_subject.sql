-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #302's review, pair (b): the subject of a BETWEEN was not read. Over timestamp bounds it is a timestamp
--   in both statements, so $1::date truncates it
-- witness: t = {(1, lo '2020-01-01 00:00', hi '2020-01-02 00:00', d '2020-01-01')}, $1 = '2020-01-01 12:00:00': A
--   compares d with $1::date and returns 1. B compares it with the timestamp and returns no row
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE $1 BETWEEN lo AND hi AND d = $1::date;
SELECT id FROM t WHERE $1 BETWEEN lo AND hi AND d = $1;
