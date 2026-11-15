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
-- origin: issue #174: the same COALESCE under NOT, inside an aggregate's CASE
-- argument: `a = $1` and `$1 = a` are one comparison, so each row is counted on both sides or on neither
create table "t" ("id" INTEGER PRIMARY KEY, "a" TEXT);
SELECT count(CASE WHEN NOT COALESCE("a" = $1, $2) THEN 1 END) AS "n" FROM "t";
SELECT count(CASE WHEN NOT COALESCE($1 = "a", $2) THEN 1 END) AS "n" FROM "t";
