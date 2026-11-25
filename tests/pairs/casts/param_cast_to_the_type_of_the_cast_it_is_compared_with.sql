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
-- origin: issue #299, the positive side: a parameter compared with a cast has the cast's type, so a cast of it
--   to that type is still dropped
-- argument: in B Postgres types $1 a date from ts::date = $1, and in A the cast makes it one. The two compare
--   the date of ts with the same value
CREATE TABLE t (id int PRIMARY KEY, ts timestamp);
SELECT id FROM t WHERE ts::date = $1::date;
SELECT id FROM t WHERE ts::date = $1;
