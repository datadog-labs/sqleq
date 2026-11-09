-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #203: the declared catalog found a table by its lower-cased name, so "S"."t" was
--   read as the declared s.t, key and all
-- witness: schemas s and "S" each with a table t; s.t = {(1, 1)}, "S".t = {}: A returns no rows, B
--   returns 1
create table "s"."t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
SELECT DISTINCT "a" FROM "S"."t";
SELECT "a" FROM "s"."t";
