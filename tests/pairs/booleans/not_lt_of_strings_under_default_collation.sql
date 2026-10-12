-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #92 item 1 (control): under the database's default collation an order comparison of strings is q_str_lt or q_str_le, which a NOT is not put through, so the pair stays unproved
-- argument: >= is the negator of < on text under every collation: when s is NULL both sides are NULL, and otherwise one is true exactly when the other is false
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT);
SELECT "id" FROM "t" WHERE NOT ("s" < 'b');
SELECT "id" FROM "t" WHERE "s" >= 'b';
