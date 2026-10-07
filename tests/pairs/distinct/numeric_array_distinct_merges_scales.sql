-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved !known-unsound
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #96: an array is opaque, and only an array of a type whose = is identity is listed as such; numeric[] compares its elements with numeric =, so DISTINCT over it stays refused by sqleq-solver (issue #86)
-- witness: t = {(1, '{2.0}'), (2, '{2.00}')}: the inner DISTINCT keeps one of the two, so A returns one row; B returns two, '{2.0}' and '{2.00}'
-- The qed pin is the QED prover deduplicating by identity on every type, the frontend half of issue #86.
create table "t" ("id" INTEGER, "a" NUMERIC[]);
SELECT DISTINCT CAST("a" AS TEXT) FROM (SELECT DISTINCT "a" FROM "t") AS "s";
SELECT DISTINCT CAST("a" AS TEXT) FROM "t";
