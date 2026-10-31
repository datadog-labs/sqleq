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
-- origin: RANK() over one table is lowered as one more than the rows of its partition sorting
--   strictly before the row (descending (nulls first)); checked on Postgres 17 by sqleq-fuzz
-- argument: rank is 1 + the number of rows that precede the row in its partition, peers sharing
--   it; the subquery counts exactly those, each key in its direction and nulls order
create table t (a integer, b integer, c integer);
SELECT a, b, RANK() OVER (ORDER BY b DESC) AS r FROM t;
SELECT a, b, 1 + (SELECT COUNT(*) FROM t AS w WHERE (((w.b > t.b)) OR (w.b IS NULL AND t.b IS NOT NULL) OR (w.b IS NOT DISTINCT FROM t.b AND FALSE))) AS r FROM t;
