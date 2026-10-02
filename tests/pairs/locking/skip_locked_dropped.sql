-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect lean: unsupported
-- origin: a row-locking clause was dropped, so the two sides lowered alike (#16)
-- witness: while another transaction holds a row lock on a row of t, A skips that row and B returns it
create table "t" ("id" INTEGER, "a" INTEGER, unique ("id"));
SELECT "a" FROM "t" FOR UPDATE SKIP LOCKED;
SELECT "a" FROM "t";
