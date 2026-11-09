-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #183: an alias column list shorter than the derived table's output was refused
-- argument: the list renames the first column and keeps b, so s.x is t's a and s.b is t's b
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "x", "b" FROM (SELECT "a", "b" FROM "t") AS "s"("x");
SELECT "a", "b" FROM "t";
