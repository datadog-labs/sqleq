-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #51: var_pop, an aggregate on no list, was lowered as a per-row scalar
-- witness: t = {}: A yields 1 (an aggregate without GROUP BY returns one row), B yields 0
create table "t" ("a" INTEGER);
SELECT count(*) FROM (SELECT var_pop("a") AS "v" FROM "t") AS "q";
SELECT count(*) FROM "t";
