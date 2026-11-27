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
-- origin: #313: a NULL is a NULL whatever it is cast to, so its cast is still retargeted and the two are one term
-- argument: Postgres evaluates the two casts to the same value on every row
CREATE TABLE t (id int PRIMARY KEY, s text, u uuid);
SELECT id, NULL::json FROM t;
SELECT id, NULL::jsonb FROM t;
