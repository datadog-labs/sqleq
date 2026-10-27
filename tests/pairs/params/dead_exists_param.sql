-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: param-misaligned
-- expect qed: proved
-- expect sqleq-solver: unsupported
-- expect sqlsolver-jvm: unsupported
-- expect cvc5: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: a parameter that is only an EXISTS subquery's select-list item counted toward arity, so the
--   pair was parameter-misaligned
-- argument: EXISTS asks only whether the subquery returns a row, which no select-list item of a
--   plain SELECT decides, whatever type the application binds $1 with; so A's $1 is dead and both
--   sides are the same query in $2
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create table "s" ("id" INTEGER PRIMARY KEY, "t_id" INTEGER);
SELECT "id" FROM "t" WHERE EXISTS (SELECT $1 FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2;
SELECT "id" FROM "t" WHERE EXISTS (SELECT 1 FROM "s" WHERE "s"."t_id" = "t"."id") AND "a" = $2;
