-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #313: a uuid is printed in lower case and a name as it is written, and the two casts of one literal were
--   one term
-- witness: t = {(1, 'x', NULL)}: A yields (1, a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11). B yields the literal as written
CREATE TABLE t (id int PRIMARY KEY, s text, u uuid);
SELECT id, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::uuid FROM t;
SELECT id, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'::name FROM t;
