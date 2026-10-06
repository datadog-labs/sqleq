-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #57: SET a = DEFAULT was read as a reference to a column named default
-- witness: t = {(1, 5, 9)}: A leaves (1, 0, 9), B leaves (1, 9, 9)

create table "t" ("id" INTEGER, "a" INTEGER DEFAULT 0, "default" INTEGER);
UPDATE "t" SET "a" = DEFAULT;
UPDATE "t" SET "a" = "default";
