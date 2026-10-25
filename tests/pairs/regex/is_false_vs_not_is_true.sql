-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- origin: issue #186: `NOT` over a demoted regex operator was emitted applied to a VARBINARY term, which
--   panicked the QED prover
-- argument: `a ~ 'y'` is `NOT (a !~ 'y')`, so A's `IS FALSE` and B's `IS TRUE` select the same rows; with the `NOT`
--   operand well-sorted, both sides lower to one plan
create table "t" ("a" TEXT);
SELECT * FROM "t" WHERE ("a" ~ 'y') IS FALSE;
SELECT * FROM "t" WHERE ("a" !~ 'y') IS TRUE;
