-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:parameter-misaligned
-- expect fuzz: param-misaligned
-- expect qed: no-plan
-- expect sqlsolver-rust: no-plan
-- expect sqlsolver-jvm: no-plan
-- catalog: inferred-seeded
-- origin: `$N` is bound by index across the pair, so a pair that renumbers its parameters compares statements nobody wrote
-- witness: $1 = 0, $2 = 1, t = {(1, 1, 1)}: A drops the row (a is not 0), B keeps it
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE "a" = $1 AND "b" = $2;
SELECT "id" FROM "t" WHERE "a" = $2;
