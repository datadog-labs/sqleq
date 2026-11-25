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
-- origin: issue #285, its third pair: with the cast dropped, ($1)::bigint[] was the bare $1 of the other side
-- witness: Postgres does not prepare A (UNION types bigint[] and character varying cannot be matched). B
--   prepares, and on t = {} with $1 = 'a' returns a
CREATE TABLE t (id int PRIMARY KEY, n bigint, name varchar(20));
SELECT ($1)::bigint[] AS name UNION ALL SELECT name FROM t;
SELECT $1 AS name UNION ALL SELECT name FROM t;
