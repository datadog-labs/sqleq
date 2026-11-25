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
-- catalog: inferred-seeded
-- origin: #302's review, measured on a corpus: a comparison of two bare parameters is a use the rule has to read,
--   or a pair that compares its parameters with each other loses its proof to the refusal
-- argument: s = $1 makes $1 a text, and $1 = $2 then makes $2 one, in both statements, so the cast is the identity
CREATE TABLE t (id int PRIMARY KEY, a int, b int, s text, d date, d2 date, ts timestamp, lo timestamp, hi timestamp);
SELECT id FROM t WHERE s = $1::text AND $1 = $2;
SELECT id FROM t WHERE s = $1 AND $1 = $2;
