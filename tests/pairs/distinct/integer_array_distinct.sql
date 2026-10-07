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
-- origin: issue #96: DISTINCT over an opaque column is refused by sqleq-solver unless the column's = is identity (issue #86); int[] compares its bounds and its elements with integer =, which is identity, and the emitted schema says so
-- argument: two int[] values = calls equal have the same dimensions, bounds and elements, so they cast to the same
--   text and the inner DISTINCT removes no text the outer one keeps
create table "t" ("id" INTEGER, "a" INTEGER[]);
SELECT DISTINCT CAST("a" AS TEXT) FROM (SELECT DISTINCT "a" FROM "t") AS "s";
SELECT DISTINCT CAST("a" AS TEXT) FROM "t";
