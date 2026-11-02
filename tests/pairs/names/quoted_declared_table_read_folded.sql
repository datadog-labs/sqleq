-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #203: the declared catalog stored a quoted "T" lower-cased, so an unquoted t, which
--   Postgres reads as another table, was read as "T", key and all
-- witness: "T" = {}, t = {(1, 1), (1, 2)}: A returns 1 once, B twice
create table "T" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
SELECT DISTINCT "a" FROM t;
SELECT "a" FROM t;
