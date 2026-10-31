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
-- origin: PR #209 review: the expansion of a ROLLUP, written out: one GROUP BY per grouping set, the empty
--   set as GROUP BY ()
-- argument: ROLLUP(a) is the grouping sets (a) and (): the groups by a, then one row over every row
--   with a NULL; GROUP BY () is one group of an empty table too
create table t (a integer);
SELECT a FROM t GROUP BY ROLLUP(a);
SELECT a FROM t GROUP BY a UNION ALL SELECT NULL AS a FROM t GROUP BY ();
