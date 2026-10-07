-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: error
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #112: type inference matched the quoted "A" against declared columns up to
--   case, so it found m."A" and t.a and refused the column as ambiguous
-- argument: "A" names only m's column; t's column is a
create table "m" ("A" INTEGER);
create table "t" ("a" INTEGER);
SELECT "A", "t"."a" FROM "m", "t";
SELECT "m"."A", "t"."a" FROM "m", "t";
