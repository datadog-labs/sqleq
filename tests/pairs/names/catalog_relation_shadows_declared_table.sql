-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #202: Postgres searches pg_catalog before the search path, so a bare pg_attribute
--   read the catalog's relation while the frontend lowered the DDL's table and its key
-- witness: any database: pg_catalog.pg_attribute holds many rows per attname, so A returns fewer
--   rows than B (on a fresh Postgres 17 database, 1188 against 3146)
create table "pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);
SELECT DISTINCT "attname" FROM "pg_attribute";
SELECT "attname" FROM "pg_attribute";
