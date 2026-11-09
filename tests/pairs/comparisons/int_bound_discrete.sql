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
-- origin: an optimizer tightens an exclusive integer bound to an inclusive one; sqleq-solver orders integer constants by value
-- argument: no integer lies strictly between 10 and 11, so a > 10 and a >= 11 hold of the same integers, and both are NULL when a is
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "t" WHERE "a" > 10;
SELECT "id" FROM "t" WHERE "a" >= 11;
