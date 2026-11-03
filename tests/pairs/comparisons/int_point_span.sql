-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: an index span writes a = 5 as the closed range [5, 5]; sqleq-solver reads <= on integers as their order, so the range is the point
-- argument: an integer is at least 5 and at most 5 exactly when it is 5, and both comparisons are NULL when a is NULL, as a = 5 is
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "t" WHERE 5 <= "a" AND "a" <= 5;
SELECT "id" FROM "t" WHERE "a" = 5;
