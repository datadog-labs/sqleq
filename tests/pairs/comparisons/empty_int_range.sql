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
-- origin: an optimizer folds a contradictory range to an empty result; sqleq-solver's bag rung asks Z3 whether a summand's conditions can hold together
-- argument: no integer is both below 1 and above 1, so A keeps no row, and neither does B
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "t" WHERE "a" < 1 AND "a" > 1;
SELECT "id" FROM "t" WHERE false;
