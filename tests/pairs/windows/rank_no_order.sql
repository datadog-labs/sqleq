-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: RANK() over one table is lowered as one more than the rows of its partition sorting
--   strictly before the row (src/normalize.rs, lower_rank_windows); with no ORDER BY every row is its
--   partition's peer. Side B is written without the rewrite's formula
-- argument: with no ORDER BY every row of the one partition ties with every other, and a rank is one
--   more than the rows that sort strictly before the row, of which there are none
create table t (a integer, b integer, c integer);
SELECT a, RANK() OVER () AS r FROM t;
SELECT a, 1 AS r FROM t;
