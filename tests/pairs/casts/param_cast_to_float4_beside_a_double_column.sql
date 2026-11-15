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
-- origin: issue #285: a float has no class in inference, so $1::float4 was dropped as the parameter's own type
--   beside a double precision column, where the bare $1 is a double precision
-- witness: t = {(1, 0.1)}, $1 = '0.1': A compares f with 0.1::float4, which is 0.100000001490116, and returns
--   no row. B returns 1
CREATE TABLE t (id int PRIMARY KEY, f double precision);
SELECT id FROM t WHERE f = $1::float4;
SELECT id FROM t WHERE f = $1;
