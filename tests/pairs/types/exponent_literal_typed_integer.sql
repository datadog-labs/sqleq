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
-- origin: issues #61 and #56: 2e0 reaches sqleq-solver typed INTEGER, which read it through a float as the integer 2
-- witness: t = {(7)}: 2e0 is numeric, so A yields 3.5 and B yields 3
create table "t" ("a" INTEGER);
SELECT "a" / 2e0 FROM "t";
SELECT "a" / 2 FROM "t";
