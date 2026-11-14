-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #279: an oid column was an INTEGER column, so a comparison with an integer was an integer comparison.
--   Postgres has no oid-against-integer operator: it reads the integer as an oid, and -1 wraps to 4294967295
-- witness: t = {(1, 0)}: A compares o with 4294967295 and returns no row; B returns 1
CREATE TABLE t (id int PRIMARY KEY, o oid NOT NULL);
SELECT id FROM t WHERE o > -1;
SELECT id FROM t WHERE o >= 0;
