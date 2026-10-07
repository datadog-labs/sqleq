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
-- origin: issue #61: TRUE was the integer 1 in sqleq-solver, and the two sides' output types were never compared
-- witness: t = {(1, 2)}: A yields the boolean true, B the integer 1
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT TRUE FROM "t";
SELECT 1 FROM "t";
