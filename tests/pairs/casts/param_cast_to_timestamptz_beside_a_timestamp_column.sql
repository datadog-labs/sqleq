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
-- origin: issue #299: the cast to timestamptz was dropped beside a timestamp column, where the bare $1 is a
--   timestamp. A timestamptz reads an offset in its input and a timestamp drops it
-- witness: t = {(1, '2020-01-01 10:00:00')}, $1 = '2020-01-01 10:00:00+05', TimeZone UTC: in A $1 is
--   05:00 UTC, ts is read as 10:00 UTC, and A returns no row. In B $1 is the timestamp 10:00 and B returns 1
CREATE TABLE t (id int PRIMARY KEY, ts timestamp);
SELECT id FROM t WHERE ts = $1::timestamptz;
SELECT id FROM t WHERE ts = $1;
