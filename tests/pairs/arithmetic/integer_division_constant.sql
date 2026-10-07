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
-- origin: issue #55: integer / reached the QED prover as z3's Euclidean div, which makes (-7) / 2
--   -4 where Postgres makes it -3
-- witness: t = {(1)}: A yields -3 (Postgres truncates toward zero), B yields -4
create table "t" ("a" INTEGER);
SELECT (-7) / 2 FROM "t";
SELECT -4 FROM "t";
