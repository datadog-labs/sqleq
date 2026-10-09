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
-- origin: ALTER INDEX ... RENAME renamed every index of the bare name, so after renaming s1.i, the
--   DROP INDEX of s2.i matched nothing and the key on s2.u outlived its index
-- witness: s2.u = {(1, 5), (2, 5)}: A yields 5 twice, B once
create table "s1"."t" ("id" INTEGER NOT NULL, "a" INTEGER NOT NULL);
create table "s2"."u" ("id" INTEGER NOT NULL, "a" INTEGER NOT NULL);
create unique index "i" on "s1"."t" ("a");
create unique index "i" on "s2"."u" ("a");
alter index "s1"."i" rename to "j";
drop index "s2"."i";
SELECT "a" FROM "s2"."u";
SELECT DISTINCT "a" FROM "s2"."u";
