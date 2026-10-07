-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #62: sqleq-fuzz ran integer `/` as floating-point division, so it could not tell it from a numeric division
-- witness: t = {(1, 1)}: A yields (1, 0), B yields (1, 0.5)
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "id", "a" / 2 AS "h" FROM "t";
SELECT "id", "a" / 2.0 AS "h" FROM "t";
