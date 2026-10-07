-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #86: sqleq-solver read = on two double precision values as identity, though 0 = -0 holds and the two cast differently
-- witness: t = {(1, 0, -0)}: a = b holds, A yields '0' and B yields '-0'
-- QED proved this pair, reading = on two double precision values as identity, until issue #84 refused a
-- cast to text over a value whose = is not identity.
create table "t" ("id" INTEGER, "a" DOUBLE PRECISION, "b" DOUBLE PRECISION);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
