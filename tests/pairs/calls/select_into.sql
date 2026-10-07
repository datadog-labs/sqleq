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
-- origin: issue #52: SELECT ... INTO was lowered as a plain SELECT
-- witness: t = {(1)}: A creates table x holding (1) and returns no rows; B returns (1)
create table "t" ("a" INTEGER);
SELECT "a" INTO "x" FROM "t";
SELECT "a" FROM "t";
