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
-- origin: #311, over a literal: '1011'::bit was retargeted as a cast with no typmod, and it has one
-- witness: t = {(1, B'1011', B'1011')}: A compares b with 1 and returns no row. B compares it with 1011 and returns 1
CREATE TABLE t (id int PRIMARY KEY, b bit(4), c bit(4));
SELECT id FROM t WHERE b = '1011'::bit;
SELECT id FROM t WHERE b = '1011'::"bit";
