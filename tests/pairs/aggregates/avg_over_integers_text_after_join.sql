-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #106: AVG over integers, typed INTEGER, had an identity =, though two equal means can print differently
-- witness: t = {(19999)}, u = {(39998), (0)}: both means are 19999, A yields '19999.0000000000000000' and B '19999.000000000000'
create table "t" ("a" INTEGER);
create table "u" ("a" INTEGER);
SELECT CAST("s"."x" AS TEXT) FROM (SELECT AVG("a") AS "x" FROM "t") AS "s" JOIN (SELECT AVG("a") AS "y" FROM "u") AS "r" ON "s"."x" = "r"."y";
SELECT CAST("r"."y" AS TEXT) FROM (SELECT AVG("a") AS "x" FROM "t") AS "s" JOIN (SELECT AVG("a") AS "y" FROM "u") AS "r" ON "s"."x" = "r"."y";
