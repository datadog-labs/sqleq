-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: found while fixing issue #61: TRUE was the integer 1 in sqleq-solver, and a function symbol did not name its argument types
-- witness: t = {(1, 2)}: to_json(TRUE) is the JSON true and to_json(1) the JSON 1
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT to_json(TRUE) FROM "t";
SELECT to_json(1) FROM "t";
