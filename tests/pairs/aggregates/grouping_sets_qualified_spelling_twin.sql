-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- origin: PR #209 review, the twin of grouping_sets_qualified_spelling_is_one_column: a and t.a are
--   one grouping column, so the select list keeps a in both sets
-- argument: Postgres resolves both spellings to one variable, so the sets are (a) twice
create table t (a integer);
SELECT a, count(*) AS n FROM t GROUP BY GROUPING SETS ((a), (t.a));
SELECT a, count(*) AS n FROM t GROUP BY a UNION ALL SELECT a, count(*) AS n FROM t GROUP BY t.a;
