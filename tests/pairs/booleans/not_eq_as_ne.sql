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
-- origin: issue #92 item 1: NOT (a = 1) was lowered as NOT over =, and a <> 1 as <>, which sqleq-solver reads as a predicate of its own, so it did not prove the pair; NOT over a native comparison is now its negator
-- argument: <> is the negator of =: when a is NULL both sides are NULL, and otherwise one is true exactly when the other is false
create table "t" ("id" INTEGER PRIMARY KEY, "a" INTEGER);
SELECT "id" FROM "t" WHERE NOT ("a" = 1);
SELECT "id" FROM "t" WHERE "a" <> 1;
