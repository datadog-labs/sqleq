-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #61: sqleq-solver's set solver made 1 and 1.0 one constant, so their text casts were one value
-- witness: t = {(1, 2)}: A yields '1.0', B yields '1'
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT DISTINCT CAST(1.0 AS TEXT) FROM "t";
SELECT DISTINCT CAST(1 AS TEXT) FROM "t";
