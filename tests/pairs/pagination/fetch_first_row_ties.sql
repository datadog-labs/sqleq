-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #63: `FETCH FIRST ROW ONLY` (no count) escaped sqleq-fuzz's literal-LIMIT guard
-- argument: as for the LIMIT (1) pair: both sides keep one arbitrary row among those with the smallest b
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "a" FROM "t" ORDER BY "b" FETCH FIRST ROW ONLY;
SELECT "a" FROM (SELECT "a", "b" FROM "t" ORDER BY "a" DESC) AS "s" ORDER BY "b" FETCH FIRST ROW ONLY;
