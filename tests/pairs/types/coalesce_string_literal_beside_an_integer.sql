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
-- origin: PR #209 review: a string literal voted for the type of a COALESCE, so COALESCE(i, '00') over an
--   INTEGER i was a string; a literal has no type of its own, and is read as the integer 0
--   (src/equality.rs, call_type; src/types.rs, coerce_select_args)
-- argument: Postgres reads the untyped literal '00' as an integer beside the integer i
create table t3 (i integer);
SELECT COALESCE(i, '00') AS v FROM t3;
SELECT COALESCE(i, 0) AS v FROM t3;
