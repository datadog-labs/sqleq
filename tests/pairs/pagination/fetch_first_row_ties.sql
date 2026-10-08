-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #63: `FETCH FIRST ROW ONLY` (no count) escaped sqleq-fuzz's literal-LIMIT guard
-- argument: as for the LIMIT (1) pair: both sides keep one arbitrary row among those with the smallest b
-- The frontend refuses the pair since issue #123: Postgres hands the outer sort B's rows in the order of B's
-- inner ORDER BY, which can decide the row kept among those tied on b, and the lowering drops that ORDER BY.
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "a" FROM "t" ORDER BY "b" FETCH FIRST ROW ONLY;
SELECT "a" FROM (SELECT "a", "b" FROM "t" ORDER BY "a" DESC) AS "s" ORDER BY "b" FETCH FIRST ROW ONLY;
