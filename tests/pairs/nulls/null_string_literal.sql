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
-- origin: issue #53: the string literal 'NULL' was a constant named NULL, which both provers read
--   as SQL NULL
-- witness: t = {('NULL')}: A yields one row, B yields none
create table "t" ("s" VARCHAR);
SELECT "s" FROM "t" WHERE "s" = 'NULL';
SELECT "s" FROM "t" WHERE false;
