-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #94: sqleq-fuzz read a SERIAL column as a nullable integer and drew NULLs into it
-- argument: SERIAL is shorthand for integer NOT NULL DEFAULT nextval(..), so no instance Postgres
--   accepts has a NULL id and the filter keeps every row

create table "t" ("id" SERIAL, "a" INTEGER);
SELECT "a" FROM "t" WHERE "id" IS NOT NULL;
SELECT "a" FROM "t";
