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
-- origin: issue #86: sqleq-solver read = on two arrays (VARBINARY) as identity, but array = compares elements with numeric =, and 2.0 = 2.00
-- witness: t = {(1, '{2.0}', '{2.00}')}: a = b holds, A yields '{2.0}' and B yields '{2.00}'
-- QED proved this pair, reading = on two numeric arrays as identity, until issue #84 refused a cast to
-- text over a value whose = is not identity.
create table "t" ("id" INTEGER, "a" NUMERIC[], "b" NUMERIC[]);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
