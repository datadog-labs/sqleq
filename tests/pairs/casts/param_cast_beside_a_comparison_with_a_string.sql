-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #302's review, pair (c): a comparison with a string constant was not read. Postgres compares two
--   operands of unknown type as text, so $1 is a text in both statements, and '1' is not '01' though 1 is 01
-- witness: t = {(1, a 1)}, $1 = '1': A compares '1' with '01' and returns no row. B returns 1
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE $1 = '01' AND a = $1::int;
SELECT id FROM t WHERE $1 = '1' AND a = $1::int;
