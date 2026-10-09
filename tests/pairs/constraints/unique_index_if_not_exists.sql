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
-- origin: issue #92: CREATE UNIQUE INDEX IF NOT EXISTS creates nothing when the name is taken, so it
--   gives no key
-- witness: t = {(1, 0), (1, 0)}: i is the non-unique index, so A yields 1 twice and B once
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER);
create index "i" on "t" ("a");
create unique index if not exists "i" on "t" ("id");
SELECT "id" FROM "t";
SELECT DISTINCT "id" FROM "t";
