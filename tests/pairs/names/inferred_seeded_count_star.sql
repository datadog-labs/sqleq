-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #111: the seeded mode refused the pair as no base tables, because no column of t
--   is named and type inference could synthesize no table, though it lowers against the declared
--   catalog and the synthesized one was discarded
-- argument: SELECT * FROM t returns t's rows, so both sides count them
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT count(*) FROM "t";
SELECT count(*) FROM (SELECT * FROM "t") AS "x";
