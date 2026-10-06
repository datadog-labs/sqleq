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
-- origin: issue #53: the string literal 'null' was a constant named null, which the QED prover
--   reads as SQL NULL
-- witness: t = {('a')}: A yields no rows ('null' IS NULL is false), B yields ('a')
create table "t" ("s" VARCHAR);
SELECT "s" FROM "t" WHERE 'null' IS NULL;
SELECT "s" FROM "t";
