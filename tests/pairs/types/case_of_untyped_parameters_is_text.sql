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
-- origin: issue #214: a CASE whose results are all untyped parameters had no type and fell to the opaque one, so a
--   set operation pairing it with a text column was refused as one over two types. Postgres resolves inputs that are
--   all of unknown type as text (PREPARE reports parameter_types = {text,text,text,text} and result_types = {text})
-- argument: the two differ only in t.name for name, which name one column of the one table in scope
CREATE TABLE t (id int PRIMARY KEY, name varchar(20));
SELECT CASE WHEN $1 = $2 THEN $3 ELSE $4 END AS name UNION ALL SELECT name FROM t;
SELECT CASE WHEN $1 = $2 THEN $3 ELSE $4 END AS name UNION ALL SELECT t.name FROM t;
