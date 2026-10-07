-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #84: an interval literal added to a timestamp is read through its spelling, and two queries that add the same one still compare
-- argument: the two filters are one conjunction written in two orders
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, "ts" TIMESTAMP);
SELECT "id", "ts" + INTERVAL '1 day' FROM "t" WHERE "a" = 1 AND "b" = 2;
SELECT "id", "ts" + INTERVAL '1 day' FROM "t" WHERE "b" = 2 AND "a" = 1;
