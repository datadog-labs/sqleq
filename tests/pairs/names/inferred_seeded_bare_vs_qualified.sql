-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: error
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: issue #111: the seeded mode refused the pair as table without referenced columns: t,
--   blaming type inference for a table the DDL declares, because t is read only through * once the
--   ORDER BY is dropped. The declared catalog lowers it to two scans of two tables. sqleq-fuzz
--   answers error on it, since it moves the bare t into schema s and then cannot create it
--   (issue #125)
-- witness: under the default search_path, t = {(1, 1)}, s.t = {}: A returns (1, 1), B no rows
create table "t" ("a" INTEGER, "b" INTEGER);
create table "s"."t" ("a" INTEGER, "b" INTEGER);
SELECT * FROM "t" ORDER BY "a";
SELECT "a", "b" FROM "s"."t" ORDER BY "a", "b";
