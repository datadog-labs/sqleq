-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

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
