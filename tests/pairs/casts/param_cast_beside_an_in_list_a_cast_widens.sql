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
-- origin: #298's review: an item of an IN list was read as the column's type. Postgres resolves one common type
--   over the column and the items, and the float8 item makes $1 a double precision in both statements
-- witness: t = {(1, 5, 0.1)}, $1 = '0.1', $2 = '5': A compares r2 with 0.1::real, which is what r2 holds, and
--   returns 1. B compares it with the double precision 0.1, which it is not, and returns no row
CREATE TABLE t (id int PRIMARY KEY, r real, r2 real);
SELECT id FROM t WHERE r IN ($1, $2::float8) AND r2 = $1::real;
SELECT id FROM t WHERE r IN ($1, $2::float8) AND r2 = $1;
