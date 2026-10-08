-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #70: TRY_CAST, which is not Postgres syntax, was lowered as a plain CAST, so the two
--   sides lowered to one plan
-- witness: t = {}: Postgres rejects A (TRY_CAST is a syntax error there) and B returns no rows.
--   Under TRY_CAST's own meaning, t = {(1, 'x')} gives NULL in A where B raises an error.
create table "t" ("id" INTEGER, "b" VARCHAR, unique ("id"));
SELECT "id", TRY_CAST("b" AS integer) FROM "t";
SELECT "id", CAST("b" AS integer) FROM "t";
