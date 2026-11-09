-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #202: a DDL that declares a table under pg_catalog was read as that table, though
--   Postgres refuses to create it and the queries read the catalog's relation
-- witness: any database: pg_catalog.pg_attribute holds many rows per attname, so A returns fewer
--   rows than B
create table "pg_catalog"."pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);
SELECT DISTINCT "attname" FROM "pg_catalog"."pg_attribute";
SELECT "attname" FROM "pg_catalog"."pg_attribute";
