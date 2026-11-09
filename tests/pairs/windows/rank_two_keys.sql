-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: RANK() over one table is lowered as one more than the rows of its partition sorting
--   strictly before the row (src/normalize.rs, lower_rank_windows), with two keys in opposite
--   directions. Side B is written without the rewrite's formula
-- argument: with a, b and c not null (so the NULLS clauses do not matter), a row is ahead of this one
--   if it has the same a and either a smaller b, or the same b and a larger c; the rank is one more
--   than the number of such rows
create table t (a integer not null, b integer not null, c integer not null);
SELECT a, RANK() OVER (PARTITION BY a ORDER BY b ASC NULLS FIRST, c DESC NULLS LAST) AS r FROM t;
SELECT a, (SELECT COUNT(*) FROM t AS w WHERE w.a = t.a AND (w.b < t.b OR (w.b = t.b AND w.c > t.c))) + 1 AS r FROM t;
