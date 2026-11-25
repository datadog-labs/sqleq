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
-- origin: issue #285, the positive side: a bare parameter stored in a column has the column's type, so a cast
--   of it to that type is still dropped
-- argument: j is a jsonb, so Postgres types the bare $1 of B a jsonb from the assignment, and in A the cast
--   makes it one. The two store the same value in the same rows
CREATE TABLE t (id int PRIMARY KEY, j jsonb);
UPDATE t SET j = $1::jsonb WHERE id = $2;
UPDATE t SET j = $1 WHERE id = $2;
