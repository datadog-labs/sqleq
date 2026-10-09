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
-- origin: index names were compared lower-cased, so "Ix" and ix were one name: renaming "Ix" renamed
--   both, and dropping ix then dropped neither, leaving a key on b
-- witness: t = {(1, 1, 5), (2, 2, 5)}: b is not unique once ix is dropped, so A yields 5 twice and B once
create table "t" ("id" INTEGER NOT NULL, "a" INTEGER NOT NULL, "b" INTEGER NOT NULL);
create unique index "Ix" on "t" ("a");
create unique index "ix" on "t" ("b");
alter index "Ix" rename to "j";
drop index "ix";
SELECT "b" FROM "t";
SELECT DISTINCT "b" FROM "t";
