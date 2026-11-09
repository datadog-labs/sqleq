-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: RANK() over one table (src/normalize.rs, lower_rank_windows): where a NULL key sorts
--   changes the ranks, and the rewrite gives each clause its own nulls placement
-- witness: t = {(1), (NULL)}: A returns (1, 1), (NULL, 2); B returns (1, 2), (NULL, 1)
create table t (b integer);
SELECT b, RANK() OVER (ORDER BY b) AS r FROM t;
SELECT b, RANK() OVER (ORDER BY b NULLS FIRST) AS r FROM t;
