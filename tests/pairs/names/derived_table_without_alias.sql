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
-- origin: issue #192: a derived table with no alias, valid since Postgres 16, was refused
-- argument: the derived table selects every row of t's column a, and nothing else reads it
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT "a" FROM (SELECT "a" FROM "t");
SELECT "a" FROM "t";
