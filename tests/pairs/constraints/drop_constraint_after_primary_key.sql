-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: an ALTER TABLE after the CREATE TABLE was not read, so a primary key a later DROP
--   CONSTRAINT dropped was still taken for a key
-- witness: t = {(1, 1), (1, 2)}: A yields 1 once, B twice
create table "t" ("id" INTEGER, "a" INTEGER, constraint "t_pk" primary key ("id"));
alter table "t" drop constraint "t_pk";
SELECT DISTINCT "id" FROM "t";
SELECT "id" FROM "t";
