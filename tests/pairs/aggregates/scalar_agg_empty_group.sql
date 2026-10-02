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
-- origin: QED read an aggregate grouped by a key the filter pins to one value as a scalar aggregate
-- witness: an empty t: A returns no rows (no group), B returns one row (0)

-- A scalar aggregate returns one row even over empty input; a grouped one returns one
-- row per group, so none. Fixed in the QED prover (empty grouping set).
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT count("b") FROM "t" WHERE "a" = 5 GROUP BY "a";
SELECT count("b") FROM "t" WHERE "a" = 5;
