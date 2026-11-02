-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #203, the negative: a table renamed to "U" is read by "U"
-- argument: both sides read the renamed table, whose key a makes the DISTINCT redundant
create table "t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
alter table "t" rename to "U";
SELECT DISTINCT "a" FROM "U";
SELECT "a" FROM "U";
