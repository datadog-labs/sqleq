-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #57: strip_identical_pagination compared the key A with the alias "A" as text, so it
--   counted A's key as projected and dropped the pagination; unquoted A is the input column a
-- witness: t = {(1, 2), (2, 1)}: A returns 2, B returns 1

-- With the pagination kept, qed still proved the pair through a second defect of the same issue in
-- lowering, where the output alias "A" was stored lower-cased, so the ORDER BY key A resolved to it
-- on both sides instead of to the input columns t.a and v.a. Output names now fold as Postgres
-- folds them.

create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "b" AS "A" FROM "t" ORDER BY A LIMIT 1;
SELECT "b" AS "A" FROM (SELECT "b", "b" AS "a" FROM "t") AS "v" ORDER BY A LIMIT 1;
