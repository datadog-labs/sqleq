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
-- origin: #302's review, pair (g): u.* was counted as one output, so the value after it was given column f4's type
--   and not f8's, and $1::float4 was dropped as the parameter's own type
-- witness: u = {(1, 0)}, $1 = '0.1': in A $1 is a real, and f8 stores 0.10000000149011612. In B it is a double
--   precision, and f8 stores 0.1
CREATE TABLE u (x int, y real);
CREATE TABLE t (a int, f4 real, f8 double precision);
INSERT INTO t (a, f4, f8) SELECT u.*, $1::float4 FROM u;
INSERT INTO t (a, f4, f8) SELECT u.*, $1 FROM u;
