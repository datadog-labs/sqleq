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
-- origin: PR #209 review: a ROLLUP over an expression and the column it reads, written out
-- argument: the sets are (a / 10, a), (a / 10) and (); a / 10 is kept whole in the second, and is
--   NULL in the third
create table t (a integer);
SELECT a / 10 AS bucket, count(*) AS n FROM t GROUP BY ROLLUP(a / 10, a);
SELECT a / 10 AS bucket, count(*) AS n FROM t GROUP BY a / 10, a UNION ALL SELECT a / 10 AS bucket, count(*) AS n FROM t GROUP BY a / 10 UNION ALL SELECT NULL AS bucket, count(*) AS n FROM t GROUP BY ();
