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
-- origin: issue #285, the positive side: the array cast under ANY is still dropped where the column is of the
--   array's element type
-- argument: n is a bigint, so in B Postgres types $1 a bigint[] from n = ANY($1), and in A the cast makes it
--   one. The two test n against the same array
CREATE TABLE t (id int PRIMARY KEY, n bigint, name varchar(20));
SELECT id FROM t WHERE n = ANY($1::bigint[]);
SELECT id FROM t WHERE n = ANY($1);
