-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #298's review: a typmod on a type inference has no class for was not seen as one, so $1::bit(3), which
--   truncates, was dropped as the parameter's own type and the pair was one plan
-- witness: t = {(1, B'101')}, $1 = '1011': $1 is a bit in both. A truncates it to 101 and returns 1. B compares b
--   with 1011 and returns no row
CREATE TABLE t (id int PRIMARY KEY, b bit(3));
SELECT id FROM t WHERE b = $1::bit(3);
SELECT id FROM t WHERE b = $1;
