-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- catalog: inferred-seeded
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #174: $2 in COALESCE(<boolean>, $2) was typed opaque, and NOT over the COALESCE
--   reached the QED prover applied to a non-boolean term, which panicked it
-- argument: `a = $1` and `$1 = a` are one comparison, so the two COALESCEs, and their negations, agree
create table "t" ("id" INTEGER PRIMARY KEY, "a" TEXT);
SELECT NOT COALESCE("a" = $1, $2) AS "x" FROM "t";
SELECT NOT COALESCE($1 = "a", $2) AS "x" FROM "t";
