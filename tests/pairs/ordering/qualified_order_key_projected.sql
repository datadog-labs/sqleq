-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect lean: unsupported
-- origin: issue #46: the control beside ordering/qualified_order_key.sql; a qualified key over the
--   column the query outputs is that output column, however it is spelled
-- argument: t.a is the only output column, and the bare a names it, so both sort by t.a
create table "t" ("k" INTEGER, "a" INTEGER);
SELECT "t"."a" FROM "t" ORDER BY "t"."a" LIMIT 1;
SELECT "t"."a" FROM "t" ORDER BY "a" LIMIT 1;
