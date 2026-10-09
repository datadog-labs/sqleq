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
-- origin: issue #91 item 4: a table declared under a schema was not found under that name, since the qualifier was stripped from the queries and not from the catalog
-- argument: both queries read s.t and keep the rows with a > 1
create table "s"."t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "s"."t" WHERE "a" > 1;
SELECT "id" FROM "s"."t" WHERE 1 < "a";
