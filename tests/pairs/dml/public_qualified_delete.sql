-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #64: sqleq-fuzz created public.t and t as two DuckDB tables and read the final state from one of them
-- argument: under the default search_path, t is public.t
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
DELETE FROM "public"."t" WHERE "a" = 1;
DELETE FROM "t" WHERE "a" = 1;
