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
-- origin: issue #58: double precision was mapped to the exact REAL, so float addition was reassociated
-- witness: t = {(0.1, 0.2, 0.3)}: A yields 0.6000000000000001, B yields 0.6
create table "t" ("x" double precision, "y" double precision, "z" double precision);
SELECT ("x" + "y") + "z" FROM "t";
SELECT "x" + ("y" + "z") FROM "t";
