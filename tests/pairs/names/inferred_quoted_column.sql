-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- catalog: inferred
-- origin: issue #112: type inference lower-cased the quoted column "createdAt", so the
--   synthesized table had createdat and lowering, which keeps the quoted case, could not find it.
--   The frontend reads no DDL under this catalog; the CREATE TABLE is there for sqleq-fuzz
-- argument: "createdAt" > 1 and 1 < "createdAt" are one comparison
create table "t" ("id" INTEGER, "createdAt" INTEGER);
SELECT "id" FROM "t" WHERE "createdAt" > 1;
SELECT "id" FROM "t" WHERE 1 < "createdAt";
