-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #285: an array cast over a bare parameter was read as stating its element's type and dropped,
--   so ($1)::bigint[] was the same node as ($1)::bigint and the pair was one plan
-- witness: Postgres does not prepare A (operator does not exist: bigint = bigint[]). B prepares, and on
--   t = {(1, 1, 'a')} with $1 = '1' returns 1
CREATE TABLE t (id int PRIMARY KEY, n bigint, name varchar(20));
SELECT id FROM t WHERE n = ($1)::bigint[];
SELECT id FROM t WHERE n = ($1)::bigint;
