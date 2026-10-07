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
-- origin: issue #87: with no COLLATE, strings compare under the database's default collation, which no input states, and two constants were ordered by code point
-- witness: in a database created with LC_COLLATE 'en_US.utf8', t = {(1)}: 'a' < 'B' holds, so A returns 1 and B returns 0 (in a C database both return 0)
create table "t" ("id" INTEGER);
SELECT CASE WHEN 'a' < 'B' THEN 1 ELSE 0 END FROM "t";
SELECT 0 FROM "t";
