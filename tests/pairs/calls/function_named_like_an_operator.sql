-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: issue #163: a call of a function named `<` was lowered as the comparison `<`
-- witness: t = {(1, 5)}: A calls the declared "<", which returns true, so A yields 1; B compares
--   5 < 1 and yields nothing
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
create function "<"(integer, integer) returns boolean language sql as $$ select true $$;
SELECT "id" FROM "t" WHERE "<"("a", 1);
SELECT "id" FROM "t" WHERE "a" < 1;
