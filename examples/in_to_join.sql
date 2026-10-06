-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: the README's quick start: an IN subquery unnested into a join
-- argument: `teams.id` is a key, so each user joins at most one team and the join repeats no user;
--   a user whose `team_id` is NULL is kept by neither side

-- Schema for the pair.
create table "users" (
  "id"       INTEGER,
  "tier"     INTEGER,
  "team_id"  INTEGER,
  unique ("id")
);
create table "teams" (
  "id"    INTEGER PRIMARY KEY,
  "plan"  INTEGER
);

-- An equivalent rewrite, but only because of the schema:
--   A: the users whose team is on plan 2, as a subquery
SELECT "id" FROM "users"
 WHERE "team_id" IN (SELECT "id" FROM "teams" WHERE "plan" = 2);
--   B: unnested into a join, as a query optimizer would
SELECT "users"."id" FROM "users" JOIN "teams" ON "teams"."id" = "users"."team_id"
 WHERE "teams"."plan" = 2;
