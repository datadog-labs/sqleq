-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #202: only pg_catalog, information_schema and pg_temp stopped the schema strip, so
--   pg_temp_3.t was stripped to t and read as the DDL's table and its key
-- witness: a session whose temporary schema is pg_temp_3, holding a temporary t = {(1), (1)}: A
--   returns 1 once and B twice
create table "t" ("a" INTEGER PRIMARY KEY);
SELECT DISTINCT "a" FROM "pg_temp_3"."t";
SELECT "a" FROM "pg_temp_3"."t";
