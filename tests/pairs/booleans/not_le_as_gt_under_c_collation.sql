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
-- origin: issue #92 item 1: under COLLATE "C" an order comparison of strings stays native, so NOT (s <= 'b') is lowered as s > 'b'
-- argument: under C strings are ordered by their bytes, a total order, and > is the negator of <=: when s is NULL both sides are NULL, and otherwise one is true exactly when the other is false
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT COLLATE "C");
SELECT "id" FROM "t" WHERE NOT ("s" <= 'b');
SELECT "id" FROM "t" WHERE "s" > 'b';
