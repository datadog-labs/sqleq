-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #285: a typmod on an array's element was not seen as one, so $1::varchar(1)[], which truncates
--   each element, was dropped like $1::text[] and the pair was one plan
-- witness: t = {(1, 'a')}, $1 = '{ab}': A truncates the element to a and returns 1. B returns no row
CREATE TABLE t (id int PRIMARY KEY, s text);
SELECT id FROM t WHERE s = ANY($1::varchar(1)[]);
SELECT id FROM t WHERE s = ANY($1::text[]);
