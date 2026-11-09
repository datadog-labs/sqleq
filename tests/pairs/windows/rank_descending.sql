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
--   strictly before the row (src/normalize.rs, lower_rank_windows); descending puts nulls first.
--   Side B is written without the rewrite's formula
-- argument: a row's rank is one more than the number of rows ahead of it; descending, a row is ahead
--   if its b is larger, or if its b is null and this row's is not (nulls sort first); a null row has
--   none ahead of it, being a peer of every other null
create table t (a integer, b integer, c integer);
SELECT a, b, RANK() OVER (ORDER BY b DESC) AS r FROM t;
SELECT a, b, (SELECT COUNT(*) FROM t AS w WHERE w.b > t.b OR (w.b IS NULL AND t.b IS NOT NULL)) + 1 AS r FROM t;
