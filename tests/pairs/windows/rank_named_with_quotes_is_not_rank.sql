-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: RANK() over one table (src/normalize.rs, lower_rank_windows): a quoted "RANK" is a function
--   of that name, which Postgres does not have, and not the window function
-- witness: Postgres rejects A with `function RANK() does not exist`; B is the window function
create table t (b integer);
SELECT b, "RANK"() OVER (ORDER BY b) AS r FROM t;
SELECT b, RANK() OVER (ORDER BY b) AS r FROM t;
