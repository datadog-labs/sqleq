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
-- origin: issue #54: a nullable UNIQUE column was sent to the QED prover as a key, which admits one
--   row per key value, NULL included
-- witness: t = {(NULL), (NULL)}: A yields two rows, B yields one (UNIQUE admits any number of NULLs)
create table "t" ("u" INTEGER, unique ("u"));
SELECT "u" FROM "t";
SELECT DISTINCT "u" FROM "t";
