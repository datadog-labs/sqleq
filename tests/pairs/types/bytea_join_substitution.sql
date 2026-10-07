-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #96: sqleq-solver read = on every opaque (VARBINARY) column through a key, so t.h = u.h no longer let it project either side; bytea's = is byte identity, and the emitted schema now says so
-- argument: bytea equality is byte-for-byte, so t.h = u.h means t.h and u.h are the same value; the two
--   projections return the same bag
create table "t" ("id" INTEGER, "h" BYTEA, unique ("id"));
create table "u" ("id" INTEGER, "h" BYTEA, unique ("id"));
SELECT "t"."h" FROM "t" JOIN "u" ON "t"."h" = "u"."h";
SELECT "u"."h" FROM "t" JOIN "u" ON "t"."h" = "u"."h";
