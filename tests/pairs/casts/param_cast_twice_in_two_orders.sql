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
-- origin: issue #285: both array casts over $1 were dropped, the pair lowered to two conjuncts in two orders,
--   and sqleq-solver proved it. Postgres types $1 at its first use and converts at the other cast
-- witness: t = {(1, 1, '01')}, $1 = '{01}': in A $1 is an integer[], {1}, which as text[] is {1}, and name is
--   not '1', so A returns no row. In B $1 is a text[], {01}, name is in it and n is in {01}::int[], so B returns 1
CREATE TABLE t (id int PRIMARY KEY, n bigint, name varchar(20));
SELECT id FROM t WHERE n = ANY($1::int[]) AND name = ANY($1::text[]);
SELECT id FROM t WHERE name = ANY($1::text[]) AND n = ANY($1::int[]);
