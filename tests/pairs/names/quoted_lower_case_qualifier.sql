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
-- origin: issue #203, the negative: a quoted part already in lower case is the name an unquoted one
--   folds to, so "s"."t" and s.t are one table
-- argument: both sides read the one table s.t, whose key a makes the DISTINCT redundant
create table s.t ("a" INTEGER PRIMARY KEY, "b" INTEGER);
SELECT DISTINCT "a" FROM "s"."t";
SELECT "a" FROM s.t;
