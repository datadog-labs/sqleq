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
-- origin: PR #249 review: a guard on the type of GREATEST and LEAST over two types. Postgres brings an
--   INTEGER beside a numeric to numeric, and so does the lowering (src/equality.rs, call_type;
--   src/types.rs, coerce_select_args). Were the call typed as its first argument, the literal would
--   be read as the integer 2 and the two sides would be two plans
-- argument: Postgres casts the integer i to numeric beside the numeric 1.5 in each call, which is what
--   the right side writes out
create table t (i integer);
SELECT GREATEST(i, 1.5) AS g, LEAST(i, 1.5) AS l FROM t;
SELECT GREATEST(CAST(i AS NUMERIC), 1.5) AS g, LEAST(CAST(i AS NUMERIC), 1.5) AS l FROM t;
