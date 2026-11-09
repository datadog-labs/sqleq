-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: reflexive
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: PR #209 (canonical_aliases): two queries that differ only in their table aliases' names
--   normalize to one query, so a pair the lowering refuses (here for its window) is still credited
--   reflexive
-- argument: renaming a query's aliases consistently does not change its rows
create table t (a integer, b integer);
SELECT e.a, sum(e.b) OVER (PARTITION BY e.a) AS s FROM t AS e;
SELECT f.a, sum(f.b) OVER (PARTITION BY f.a) AS s FROM t AS f;
