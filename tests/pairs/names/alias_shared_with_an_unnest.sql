-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: PR #209 review: canonical_aliases collected table and derived-table aliases only, so an UNNEST
--   aliased like a table escaped its duplicate check while a qualifier naming it was renamed, and
--   two queries Postgres tells apart were one tree, credited reflexive (src/normalize.rs)
-- witness: t = {(1)}: A returns 5, B returns 1
create table t (a integer);
SELECT (SELECT x.a FROM unnest(ARRAY[5]) AS x(a)) AS r FROM t AS x;
SELECT (SELECT y.a FROM unnest(ARRAY[5]) AS x(a)) AS r FROM t AS y;
