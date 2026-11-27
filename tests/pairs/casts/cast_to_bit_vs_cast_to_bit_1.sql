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
-- origin: #311: an unquoted bit with no length is bit(1), so the two casts are one function
-- argument: Postgres reads the two targets as one type, so the two statements are the same query
CREATE TABLE t (id int PRIMARY KEY, b bit(4), c bit(4));
SELECT id FROM t WHERE b = c::bit;
SELECT id FROM t WHERE b = c::bit(1);
