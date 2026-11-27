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
-- origin: #313: a literal cast to a type with no class was CAST(lit AS VARBINARY) whatever the type, so the cast to
--   "char", which keeps one byte, was the cast to name
-- witness: t = {(1, 'A', NULL)}: A compares s with 'A' and returns 1. B compares it with 'ABC' and returns no row
CREATE TABLE t (id int PRIMARY KEY, s text, u uuid);
SELECT id FROM t WHERE s = 'ABC'::"char";
SELECT id FROM t WHERE s = 'ABC'::name;
