-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqlsolver-rust: proved-literal
-- expect sqlsolver-jvm: proved-literal
-- expect lean: unsupported
-- origin: the README's quick start: IN against the OR it abbreviates
-- argument: `tier IN (1, 2)` is defined as `tier = 1 OR tier = 2`

-- Schema for the pair.
create table "users" (
  "id"      INTEGER,
  "tier"    INTEGER,
  "org_id"  INTEGER,
  unique ("id")
);

-- Two query rewrites we claim are equivalent:
--   A: the membership test written as an OR of equalities
SELECT "id", "tier" FROM "users"
 WHERE "org_id" = 1 AND ("tier" = 1 OR "tier" = 2);
--   B: the same test written with IN
SELECT "id", "tier" FROM "users"
 WHERE "org_id" = 1 AND "tier" IN (1, 2);
