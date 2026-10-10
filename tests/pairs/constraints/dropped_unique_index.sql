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
-- origin: issue #92: a unique index a later DROP INDEX drops gives no key
-- witness: t = {(1, 0), (1, 0)}: A yields 1 twice, B once
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER);
create unique index "t_id" on "t" ("id");
drop index "t_id";
SELECT "id" FROM "t";
SELECT DISTINCT "id" FROM "t";
