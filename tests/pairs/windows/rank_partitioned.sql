-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: RANK() over one table is lowered as one more than the rows of its partition sorting
--   strictly before the row (src/normalize.rs, lower_rank_windows), partitioned. Side B is written
--   without the rewrite's formula
-- argument: with a and b not null, a row's rank is one more than the number of rows with the same a
--   and a smaller b; rows with an equal b are peers and share it
create table t (a integer not null, b integer not null, c integer);
SELECT a, b, RANK() OVER (PARTITION BY a ORDER BY b) AS r FROM t;
SELECT a, b, (SELECT COUNT(*) FROM t AS w WHERE w.a = t.a AND w.b < t.b) + 1 AS r FROM t;
