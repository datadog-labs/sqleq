-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #251: the negative side. A parameter with a type of its own is a typed value: NULLIF(i, $1::numeric)
--   over an INTEGER i compares two numerics, and cast to INTEGER the parameter would be another value. It stays
--   refused (src/types.rs, coerce_select_args)
-- witness: t = {(2)}, $1 = 1.5: A returns 2, since 2 <> 1.5; B returns NULL, since 1.5::integer is 2
create table t (i integer);
SELECT NULLIF(i, $1::numeric) AS v FROM t;
SELECT NULLIF(i, ($1::numeric)::integer) AS v FROM t;
