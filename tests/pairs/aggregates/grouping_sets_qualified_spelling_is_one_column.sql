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
-- origin: PR #209 review: grouping expressions were matched by their text, so a and t.a were two
--   columns, and the select list's a was nulled in the set that grouped by t.a
-- witness: t = {(1)}: A returns (1, 1) twice, since the two sets are one set written twice; B returns
--   (1, 1) and (NULL, 1)
create table t (a integer);
SELECT a, count(*) AS n FROM t GROUP BY GROUPING SETS ((a), (t.a));
SELECT a, count(*) AS n FROM t GROUP BY a UNION ALL SELECT CAST(NULL AS integer) AS a, count(*) AS n FROM t GROUP BY t.a;
