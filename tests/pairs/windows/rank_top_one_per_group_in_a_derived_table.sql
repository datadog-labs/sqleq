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
-- origin: a RANK() in a derived table, filtered to the first of each group outside it. The outer
--   select reads a derived table, which the guard of the RANK() rewrite leaves alone; the derived
--   table's own select reads one table and nothing else, and is rewritten against that table
--   (src/normalize.rs, lower_rank_windows). Side B is written without the rewrite's formula
-- argument: with g and x not null, a row's rank within its g is 1 exactly when no row of the same g
--   has a smaller x, and the filter keeps only the rows ranked 1, so B writes r as 1
create table t (id integer primary key, g integer not null, x integer not null);
SELECT * FROM (SELECT id, g, x, RANK() OVER (PARTITION BY g ORDER BY x) AS r FROM t) AS s WHERE r = 1;
SELECT id, g, x, 1 AS r FROM t AS t1 WHERE NOT EXISTS (SELECT 1 FROM t AS t2 WHERE t2.g = t1.g AND t2.x < t1.x);
