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
-- origin: PR #209 review, the twin of rollup_integer_column_null_beside_a_string_literal: the left-out
--   column's NULL, written as the typed NULL it is
-- argument: the total row's a is a NULL integer, so COALESCE reads '00' as the integer 0
create table t (a integer);
SELECT COALESCE(a, '00') = '0' AS z, count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT COALESCE(a, '00') = '0' AS z, count(*) AS n FROM t GROUP BY a UNION ALL SELECT COALESCE(CAST(NULL AS integer), '00') = '0' AS z, count(*) AS n FROM t GROUP BY ();
