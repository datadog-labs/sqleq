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
-- origin: issue #299: the cast to timestamp was dropped beside a date column, where the bare $1 is a date
-- witness: t = {(1, '2020-01-01')}, $1 = '2020-01-01 10:00:00': in A d is compared with 10:00 that day and
--   A returns no row. In B $1 is the date 2020-01-01 and B returns 1
CREATE TABLE t (id int PRIMARY KEY, d date);
SELECT id FROM t WHERE d = $1::timestamp;
SELECT id FROM t WHERE d = $1;
