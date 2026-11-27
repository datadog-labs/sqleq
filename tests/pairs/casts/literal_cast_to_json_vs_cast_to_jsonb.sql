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
-- origin: #313: json keeps the text it is given and jsonb parses it, and the two casts of one literal were one term
-- witness: t = {(1, 'x', NULL)}: A yields (1, {"a":1, "a":2}), with both keys. B yields (1, {"a": 2})
CREATE TABLE t (id int PRIMARY KEY, s text, u uuid);
SELECT id, '{"a":1, "a":2}'::json FROM t;
SELECT id, '{"a":1, "a":2}'::jsonb FROM t;
