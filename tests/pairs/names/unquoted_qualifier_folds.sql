-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #203, the negative: an unquoted S.t folds to s.t, so it is the declared table
-- argument: both sides read the one table s.t, whose key a makes the DISTINCT redundant
create table "s"."t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
SELECT DISTINCT "a" FROM S.t;
SELECT "a" FROM "s"."t";
