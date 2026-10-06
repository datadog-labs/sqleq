-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- catalog: inferred
-- origin: issue #48: the per-query strip_schema guard, on the lowering path: under an inferred
--   catalog both sides lowered to a scan of one table t
-- witness: s1.t = {(1)}, s2.t = {}: A returns 1, B returns no rows

create table "s1"."t" ("a" INTEGER);
create table "s2"."t" ("a" INTEGER);
SELECT "a" FROM "s1"."t";
SELECT "a" FROM "s2"."t";
