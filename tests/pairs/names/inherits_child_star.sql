-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a table created with INHERITS was read with its own columns alone, though it has its
--   parent's too, so * read too few
-- witness: c = {(1, 2)}: A yields (1, 2), B yields (2)
create table "p" ("a" INTEGER);
create table "c" ("b" INTEGER) inherits ("p");
SELECT * FROM "c";
SELECT "b" FROM "c";
