-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit-reflexive
-- expect fuzz: no-counterexample
-- expect qed: proved-literal
-- expect sqleq-solver: proved-literal
-- expect cvc5: proved-literal
-- expect lean: unsupported
-- origin: issue #92 item 5: #> and jsonb_extract_path got two symbols, so a literal path written as
--   the operator never met the same path written as the call
-- argument: Postgres implements jsonb #> as jsonb_extract_path (pg_operator.oprcode), and the literal
--   '{a,b}' is the text[] {a, b}, so A is jsonb_extract_path("j", VARIADIC '{a,b}'), which is B. Postgres 17
--   returns the same rows for both on objects, arrays, scalars and a NULL document
create table "t" ("id" INTEGER PRIMARY KEY, "j" JSONB);
SELECT "id", "j" #> '{a,b}' AS "v" FROM "t";
SELECT "id", jsonb_extract_path("j", 'a', 'b') AS "v" FROM "t";
