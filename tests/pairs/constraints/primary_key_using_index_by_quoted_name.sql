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
-- origin: index names were compared lower-cased, so PRIMARY KEY USING INDEX "Ix" took the index ix,
--   and its column b was read as NOT NULL
-- witness: t = {(1, 1, NULL)}: A yields no row, B yields 1
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER, "b" INTEGER);
create unique index "ix" on "t" ("b");
create unique index "Ix" on "t" ("a");
alter table "t" add constraint "t_pk" primary key using index "Ix";
SELECT "id" FROM "t" WHERE "b" IS NOT NULL;
SELECT "id" FROM "t";
