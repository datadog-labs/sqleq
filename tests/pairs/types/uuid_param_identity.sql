-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- catalog: inferred-seeded
-- origin: a join rewritten through a parameter: ON fact.version = version.id becomes ON version.id = $1 beside WHERE fact.version = $1. $1 meets only
--   uuid columns, so the frontend lists it in identity_params, and sqleq-solver reads its = as identity: with $1's = read through a key, the two
--   equalities to $1 did not give fact.version = version.id
-- argument: Postgres types $1 as a uuid, whose = is identity, so fact.version = $1 and version.id = $1 hold together exactly when fact.version =
--   version.id and fact.version = $1 do
create table "version" ("id" uuid PRIMARY KEY, "n" INTEGER);
create table "fact" ("id" INTEGER PRIMARY KEY, "version" uuid);
SELECT "fact"."id", "version"."n" FROM "fact" JOIN "version" ON "fact"."version" = "version"."id" WHERE "fact"."version" = $1;
SELECT "fact"."id", "version"."n" FROM "fact" JOIN "version" ON "version"."id" = $1 WHERE "fact"."version" = $1;
