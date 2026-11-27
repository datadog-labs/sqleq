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
-- origin: #313: one cast spelled twice is still one function of the literal
-- argument: Postgres evaluates the two casts to the same value on every row
CREATE TABLE t (id int PRIMARY KEY, s text, u uuid);
SELECT id FROM t WHERE u = 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::uuid;
SELECT id FROM t WHERE u = 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::"uuid";
