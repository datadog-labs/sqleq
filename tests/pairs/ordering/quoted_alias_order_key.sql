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
-- origin: issue #57: output column names are lower-cased whatever their quoting, so the
--   unquoted key A (input column a) matches the output alias "A"
-- witness: t = {(1, 2), (2, 1)}: A returns 2, B returns 1
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "b" AS "A" FROM "t" ORDER BY A LIMIT 1;
SELECT "b" AS a FROM "t" ORDER BY A LIMIT 1;
