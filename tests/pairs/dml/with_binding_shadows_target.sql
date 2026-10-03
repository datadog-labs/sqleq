-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: a WITH binding named like the DML target was read as the target (4df30d8)
-- witness: t = {(1, 0)}: a DELETE's target is always the table, so A deletes the row and B keeps it

-- The binding is visible to subqueries in the statement, never to its target.
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
WITH t AS (SELECT * FROM t WHERE a = 1) DELETE FROM t;
DELETE FROM t WHERE a = 1;
