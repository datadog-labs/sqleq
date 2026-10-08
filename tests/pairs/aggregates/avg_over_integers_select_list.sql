-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #106: AVG over an integer column was typed INTEGER in the IR, though Postgres returns numeric
-- witness: t = {(1, 0), (2, 1)}: avg(a) = 0.5, so A yields true and B yields false
create table "t" ("id" INTEGER, "a" INTEGER);
SELECT AVG("a") * 2 = 1 FROM "t";
SELECT AVG("a") <> AVG("a") FROM "t";
