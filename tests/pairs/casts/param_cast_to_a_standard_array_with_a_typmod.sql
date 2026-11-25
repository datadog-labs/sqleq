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
-- origin: #298's review: varchar(3) ARRAY is the SQL-standard spelling of varchar(3)[], and the typmod on its
--   element was not seen, so the cast was dropped
-- witness: t = {(1, '{abc}')}, $1 = '{abcd}': A truncates the element to abc and returns 1. B returns no row
CREATE TABLE t (id int PRIMARY KEY, tags varchar(3) ARRAY);
SELECT id FROM t WHERE tags = $1::varchar(3) ARRAY;
SELECT id FROM t WHERE tags = $1;
