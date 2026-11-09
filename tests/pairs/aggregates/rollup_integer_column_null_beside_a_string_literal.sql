-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: PR #209 review: the NULL of a left-out INTEGER column was a bare nullary NULL, which counts as
--   untyped, so the string literal beside it decided the type of its COALESCE
-- witness: t = {(1)}: A returns (false, 1) and (true, 1), since COALESCE(NULL::int, '00') is the
--   integer 0; B returns (false, 1) twice, since COALESCE(NULL, '00') is the text '00'
create table t (a integer);
SELECT COALESCE(a, '00') = '0' AS z, count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT COALESCE(a, '00') = '0' AS z, count(*) AS n FROM t GROUP BY a UNION ALL SELECT COALESCE(NULL, '00') = '0' AS z, count(*) AS n FROM t GROUP BY ();
