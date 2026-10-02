-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqlsolver-rust: no-proof
-- expect sqlsolver-jvm: no-proof
-- origin: the README's quick start: a rewrite that drops a predicate
-- witness: users = {(1, 1, 2)}: A drops the row (org 2), B keeps it

-- Schema for the pair.
create table "users" (
  "id"      INTEGER,
  "tier"    INTEGER,
  "org_id"  INTEGER,
  unique ("id")
);

-- A NON-equivalent rewrite: B drops the org-scoping predicate, so it returns rows A would not.
--   A: scoped to one org
SELECT "id" FROM "users" WHERE "org_id" = 1 AND "tier" = 1;
--   B: org scoping dropped
SELECT "id" FROM "users" WHERE "tier" = 1;
