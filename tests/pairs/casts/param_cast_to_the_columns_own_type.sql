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
-- origin: issue #285, the positive side: a cast to a type inference has no class for is still the parameter's
--   own type where every use of the parameter states it
-- argument: u is a uuid, so Postgres types the bare $1 of B a uuid from the comparison, and in A the cast makes
--   it one. The cast is the identity, and the two compare u with the same value
CREATE TABLE t (id int PRIMARY KEY, u uuid);
SELECT id FROM t WHERE u = $1::uuid;
SELECT id FROM t WHERE u = $1;
