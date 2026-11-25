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
-- origin: #298's review: an unquoted bit with no length is bit(1), so $1::bit truncates to one bit, and it was
--   dropped as the parameter's own type
-- witness: t = {(1, B'1')}, $1 = '1011': $1 is a bit in both. A truncates it to 1 and returns 1. B compares b with
--   1011 and returns no row
CREATE TABLE t (id int PRIMARY KEY, b bit);
SELECT id FROM t WHERE b = $1::bit;
SELECT id FROM t WHERE b = $1;
