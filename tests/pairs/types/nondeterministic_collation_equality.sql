-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #87: under a non-deterministic collation = is not identity, but the column's COLLATE was dropped
-- witness: t = {(1, 'a')}: under ci 'a' = 'A', so A returns nothing and B returns 1
create collation "ci" (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
create table "t" ("id" INTEGER, "s" TEXT COLLATE "ci");
SELECT "id" FROM "t" WHERE "s" = 'A' AND NOT "s" = 'a';
SELECT "id" FROM "t" WHERE "s" = 'A';
