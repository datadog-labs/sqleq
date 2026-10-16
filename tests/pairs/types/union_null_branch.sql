-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #177: a NULL branch is typed INTEGER, and a text column under it reached the prover
--   ill-typed
-- argument: Postgres reads the untyped NULL as text, the type the cast in B names
create table "t" ("x" TEXT);
SELECT NULL UNION ALL SELECT "x" FROM "t";
SELECT NULL::text UNION ALL SELECT "x" FROM "t";
