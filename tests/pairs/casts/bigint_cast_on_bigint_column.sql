-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: proved
-- expect lean: unsupported
-- origin: sqleq-fuzz compared result cells by DuckDB type, so an identity cast changed the cell (#6)
-- argument: casting a bigint column to bigint is the identity
create table "t" ("id" INTEGER, "parent" BIGINT, unique ("id"));
SELECT "id", "parent"::bigint AS "parent" FROM "t";
SELECT "id", "parent" FROM "t";
