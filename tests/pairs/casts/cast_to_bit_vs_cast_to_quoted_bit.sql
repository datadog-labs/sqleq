-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: #311: a cast kept as a symbol was named after its target without the quotes, so c::bit, which is
--   bit(1) and truncates, was the function c::"bit" is, which has no length and changes nothing
-- witness: t = {(1, B'1011', B'1011')}: A compares b with 1 and returns no row. B compares it with 1011 and returns 1
CREATE TABLE t (id int PRIMARY KEY, b bit(4), c bit(4));
SELECT id FROM t WHERE b = c::bit;
SELECT id FROM t WHERE b = c::"bit";
