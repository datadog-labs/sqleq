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
-- origin: issue #214: the negative side. A set operation whose branches are of two types stays refused: an array of
--   bigint is not text, whatever the parameter inside it is
-- witness: t = {}: Postgres rejects A (UNION types bigint[] and character varying cannot be matched) and B returns
--   one row
CREATE TABLE t (id int PRIMARY KEY, name varchar(20));
SELECT ARRAY[$1]::bigint[] AS name UNION ALL SELECT name FROM t;
SELECT CASE WHEN true THEN $2 ELSE $3 END AS name UNION ALL SELECT name FROM t;
