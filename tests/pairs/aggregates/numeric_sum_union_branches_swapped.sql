-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #85 (control): numeric addition is exact, so SUM over numeric is a function of the bag and stays lowered
-- argument: numeric + is exact and commutative and associative, so the sum does not depend on the order of its inputs
create table "t" ("x" NUMERIC);
create table "u" ("x" NUMERIC);
SELECT SUM("x") FROM (SELECT "x" FROM "t" UNION ALL SELECT "x" FROM "u") AS "s";
SELECT SUM("x") FROM (SELECT "x" FROM "u" UNION ALL SELECT "x" FROM "t") AS "s";
