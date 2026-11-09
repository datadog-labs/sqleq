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
-- origin: PR #249 review: NULLIF was typed as its first argument and its second argument cast to that
--   type, so NULLIF(i, 1.5) over an INTEGER i compared i with the integer 2 and was one plan with
--   NULLIF(i, 1.5::integer); Postgres compares the two at their common type, as = would, and returns
--   a numeric. A second argument that does not coerce to the first's type is refused (src/types.rs,
--   coerce_select_args)
-- witness: t = {(2)}: A returns 2, since 2 <> 1.5; B returns NULL, since 1.5::integer is 2
create table t (i integer);
SELECT NULLIF(i, 1.5) AS v FROM t;
SELECT NULLIF(i, 1.5::integer) AS v FROM t;
