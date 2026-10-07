-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #54: a nullable UNIQUE column was sent to the QED prover as a key, which admits one
--   row per key value, NULL included
-- witness: t = {(NULL, 1), (NULL, 2)}: A yields (NULL, 1) and (NULL, 2), B yields four rows
create table "t" ("u" INTEGER, "v" INTEGER, unique ("u"));
SELECT "t1"."u", "t1"."v" FROM "t" AS "t1";
SELECT "t1"."u", "t2"."v" FROM "t" AS "t1" JOIN "t" AS "t2" ON "t1"."u" IS NOT DISTINCT FROM "t2"."u";
