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
-- origin: #298's review: in an INSERT from a set operation Postgres resolves the branches against each other
--   first, so the other branch types a bare parameter, and the column it is stored in does not
-- witness: v = {}, $1 = '0.1': in B $1 is a real, from v.r, and f8 stores 0.10000000149011612. In A it is a double
--   precision, and f8 stores 0.1
CREATE TABLE v (y int, r real);
CREATE TABLE t (a int, f8 double precision);
INSERT INTO t (a, f8) SELECT 1, $1::float8 UNION ALL SELECT y, r FROM v;
INSERT INTO t (a, f8) SELECT 1, $1 UNION ALL SELECT y, r FROM v;
