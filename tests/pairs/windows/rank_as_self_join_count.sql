-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: RANK() over one table (src/normalize.rs, lower_rank_windows); side B counts the rows ahead
--   with a LEFT JOIN and a GROUP BY on the key, a different shape from the rewrite's correlated
--   count
-- argument: with id a key and a, b not null, t1 joined to the rows t2 of its own a with a smaller b
--   has one output row per t1 row, and COUNT(t2.id) counts those t2 rows (a row with none is
--   NULL-padded, and COUNT of the padded id is 0); one more than that is the rank
create table t (id integer primary key, a integer not null, b integer not null);
SELECT a, b, RANK() OVER (PARTITION BY a ORDER BY b) AS r FROM t;
SELECT t1.a, t1.b, 1 + COUNT(t2.id) AS r FROM t AS t1 LEFT JOIN t AS t2 ON t2.a = t1.a AND t2.b < t1.b GROUP BY t1.id, t1.a, t1.b;
