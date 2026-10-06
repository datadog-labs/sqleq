-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #64: sqleq-fuzz shimmed jsonb_build_object with DuckDB json_object, which keeps key order and duplicate keys
-- argument: jsonb stores an object's keys in its own order, so the order they were built in is not observable
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id", jsonb_build_object('a', "a", 'b', "id") AS "j" FROM "t";
SELECT "id", jsonb_build_object('b', "id", 'a', "a") AS "j" FROM "t";
