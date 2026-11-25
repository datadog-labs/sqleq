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
-- origin: #298's review: a parameter another use typed first widens an IN list's common type as a cast does. Here
--   f = $2 makes $2 a double precision, and the list then makes $1 one
-- witness: t = {(1, 5, 0.1, 5)}, $1 = '0.1', $2 = '5': A compares r2 with 0.1::real and returns 1. B compares it
--   with the double precision 0.1 and returns no row
CREATE TABLE t (id int PRIMARY KEY, r real, r2 real, f double precision);
SELECT id FROM t WHERE f = $2 AND r IN ($1, $2) AND r2 = $1::real;
SELECT id FROM t WHERE f = $2 AND r IN ($1, $2) AND r2 = $1;
