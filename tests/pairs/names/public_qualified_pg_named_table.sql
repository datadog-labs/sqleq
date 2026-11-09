-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #202: the control for the refusal of a bare pg_* name: a pg_* table read through
--   the schema the DDL declares it in is that table
-- argument: public.pg_attribute is the declared table, and its key makes DISTINCT redundant
create table "public"."pg_attribute" ("attname" TEXT PRIMARY KEY, "attnum" INTEGER);
SELECT DISTINCT "attname" FROM "public"."pg_attribute";
SELECT "attname" FROM "public"."pg_attribute";
