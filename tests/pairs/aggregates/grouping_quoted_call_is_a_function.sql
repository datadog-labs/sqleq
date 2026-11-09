-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: PR #209 review: a quoted "grouping"(a) is a call of a function, but was read as GROUPING and
--   replaced by its mask, so the pair was one plan
-- witness: t = {(1)}: A calls the declared function and returns (7, 1) twice; B returns (0, 1) and (1, 1)
create table t (a integer);
create function "grouping"(integer) returns integer language sql as $$ select 7 $$;
SELECT "grouping"(a) AS g, count(*) AS n FROM t GROUP BY ROLLUP(a);
SELECT GROUPING(a) AS g, count(*) AS n FROM t GROUP BY ROLLUP(a);
