-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqlsolver-rust: proved-literal
-- expect sqlsolver-jvm: proved-literal
-- origin: sqlparser 0.62 parsed the right operand of IS DISTINCT FROM at precedence 0 (docs/sqlparser-is-distinct-from.md)
-- argument: IS DISTINCT FROM binds tighter than AND, so the parentheses change nothing
create table "t" ("id" INTEGER, "a" INTEGER, "b" INTEGER, unique ("id"));
SELECT "id" FROM "t" WHERE "a" IS DISTINCT FROM 1 AND "b" = 2;
SELECT "id" FROM "t" WHERE ("a" IS DISTINCT FROM 1) AND "b" = 2;
