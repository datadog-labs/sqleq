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
-- origin: RANK() over one table (src/normalize.rs, lower_rank_windows): peers share a rank, so a count
--   that treats a peer as ahead (<= for <) is a different query
-- witness: t = {(1), (1)}: A returns (1, 1), (1, 1); B returns (1, 3), (1, 3)
create table t (b integer);
SELECT b, RANK() OVER (ORDER BY b) AS r FROM t;
SELECT b, 1 + (SELECT COUNT(*) FROM t AS w WHERE w.b <= t.b) AS r FROM t;
