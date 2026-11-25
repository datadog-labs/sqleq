-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #302's review, the positive side: a bare item of one branch of a set operation takes the type of the
--   other branch's item, and is a use the rule reads
-- argument: the branches are resolved against each other, and s is a text, so $1 is a text in both statements and
--   the cast is the identity
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT $1::text AS s UNION ALL SELECT s FROM t;
SELECT $1 AS s UNION ALL SELECT s FROM t;
