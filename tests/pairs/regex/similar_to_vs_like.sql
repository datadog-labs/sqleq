-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #64: SIMILAR TO reached DuckDB, which reads its pattern as a regular expression with no % or _ wildcards
-- argument: in SIMILAR TO, % matches any string and the match is anchored at both ends, so 'a%' means the same as in LIKE
create table "t" ("id" INTEGER, "c" TEXT, unique ("id"));
SELECT "id" FROM "t" WHERE "c" SIMILAR TO 'a%';
SELECT "id" FROM "t" WHERE "c" LIKE 'a%';
