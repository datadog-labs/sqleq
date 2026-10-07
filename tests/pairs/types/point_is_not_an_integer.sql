-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #58: point was mapped to INTEGER, because its name contains INT
-- witness: t = {('(0.1,0)', '(0.2,0)')}: A yields (0.10000000000000003,0), B yields (0.1,0)
create table "t" ("p" point, "q" point NOT NULL);
SELECT ("p" + "q") - "q" FROM "t";
SELECT "p" FROM "t";
