-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect sqlsolver-jvm: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #190: sqlparser read ONLY "t" as a table named ONLY, which resolved to the table "only"
-- witness: t = {(1, 1)}, "only" = {}: A reads t (with no children) and yields 1; B yields nothing
create table "t" ("a" INTEGER, "b" INTEGER);
create table "only" ("a" INTEGER, "b" INTEGER);
SELECT "a" FROM ONLY "t";
SELECT "a" FROM "only";
