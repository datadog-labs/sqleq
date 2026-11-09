-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: PR #209 review: a quoted "rollup"(a) is a call of a function, but was read as ROLLUP, then
--   expanded again for ever until the frontend overflowed its stack
-- witness: t = {(1), (2)}: A groups by the declared function, which is 0 for every row, and returns
--   2; B returns 1, 1 and 2
create table t (a integer);
create function "rollup"(integer) returns integer language sql as $$ select 0 $$;
SELECT count(*) AS n FROM t GROUP BY "rollup"(a);
SELECT count(*) AS n FROM t GROUP BY ROLLUP(a);
