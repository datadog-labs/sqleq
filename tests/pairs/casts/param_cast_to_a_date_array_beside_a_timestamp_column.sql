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
-- origin: issue #285: the array cast was dropped under ANY whatever the column it is compared with, so
--   ts = ANY($1::date[]) was ts = ANY($1), where $1 is a timestamp[]
-- witness: t = {(1, '2020-01-01 10:00:00')}, $1 = '{"2020-01-01 10:00:00"}': in A the element is the date
--   2020-01-01, which is midnight, and A returns no row. B returns 1
CREATE TABLE t (id int PRIMARY KEY, ts timestamp);
SELECT id FROM t WHERE ts = ANY($1::date[]);
SELECT id FROM t WHERE ts = ANY($1);
