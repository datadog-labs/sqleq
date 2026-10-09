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
-- origin: issue #125: sqleq-fuzz moved the bare t into schema s, the one schema the DDL named t by,
--   though that name was the DDL's own create table "s"."t"; the DDL then failed with relation "t"
--   already exists, and the pair got an error instead of a counterexample
-- witness: t = {(1)}, s.t = {}: A returns 1, B no rows
create table "t" ("a" INTEGER);
create table "s"."t" ("a" INTEGER);
SELECT "a" FROM "t";
SELECT "a" FROM "s"."t";
