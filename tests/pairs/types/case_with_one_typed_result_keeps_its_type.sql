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
-- origin: issue #214: the control. A CASE with one result of a type takes that type, as before: the parameter beside
--   the 0 is an integer, and the set operation is one over integers
-- argument: the two differ only in t.id for id, which name one column of the one table in scope
CREATE TABLE t (id int PRIMARY KEY, name varchar(20));
SELECT CASE WHEN $1 = $2 THEN $3 ELSE 0 END AS n UNION ALL SELECT id FROM t;
SELECT CASE WHEN $1 = $2 THEN $3 ELSE 0 END AS n UNION ALL SELECT t.id FROM t;
