-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #108: numrange was read as a type whose = is identity, though its bounds compare as numerics, so 1.0 and 1.00 are equal bounds
-- witness: t = {(1, '[1.0,2.0)', '[1.00,2.00)')}: a = b holds, A yields [1.0,2.0) and B yields [1.00,2.00)
create table "t" ("id" INTEGER, "a" numrange, "b" numrange);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
