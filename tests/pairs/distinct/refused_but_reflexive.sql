-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: reflexive
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: a pair the frontend refuses to lower -- a window function has no counterpart in the
--   prover's IR -- whose two sides are one query once the DISTINCT under IN is stripped, so it is
--   settled by reflexivity without a plan or a prover
-- argument: x IN (SELECT DISTINCT e FROM u) is x IN (SELECT e FROM u): IN tests membership, which
--   duplicates do not change, and no LIMIT, OFFSET or FETCH slices the subquery
create table "t" ("a" INTEGER);
create table "u" ("x" INTEGER);
SELECT row_number() OVER (ORDER BY "a") FROM "t" WHERE "a" IN (SELECT DISTINCT "x" FROM "u");
SELECT row_number() OVER (ORDER BY "a") FROM "t" WHERE "a" IN (SELECT "x" FROM "u");
