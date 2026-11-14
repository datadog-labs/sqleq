-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #241: IS NOT DISTINCT FROM was a sum of two cases, both NULL or both not and equal. In a join it split
--   the row into two summands, and once the key made the two rows one nothing merged them back. It is one 0/1 term now
-- argument: a.id = b.id makes a and b one row, id being the key, so b.tag is a.tag and is not distinct from it
CREATE TABLE crate (id int PRIMARY KEY, tag int, note int);
SELECT id, tag FROM crate;
SELECT a.id, b.tag FROM crate AS a JOIN crate AS b ON a.id = b.id AND a.tag IS NOT DISTINCT FROM b.tag;
