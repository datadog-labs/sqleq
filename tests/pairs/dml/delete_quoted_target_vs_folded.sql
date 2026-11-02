-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #203: the DML reduction compared two targets by their lower-cased names, so DELETE
--   FROM "T" and DELETE FROM t were read as one table
-- witness: t = {(1, 1)}, "T" = {}: A leaves t as it was, B deletes its row
create table "t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
DELETE FROM "T" WHERE "a" = 1;
DELETE FROM "t" WHERE "a" = 1;
