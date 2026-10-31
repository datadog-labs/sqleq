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
-- origin: issue #92 item 1: booleans/not_eq_as_ne.sql in a select list, where a NOT is lowered outside a predicate and a NULL result is part of the output
-- argument: <> is the negator of =: when a is NULL both sides yield NULL, and otherwise one yields true exactly when the other yields false
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id", NOT ("a" = 1) FROM "t";
SELECT "id", "a" <> 1 FROM "t";
