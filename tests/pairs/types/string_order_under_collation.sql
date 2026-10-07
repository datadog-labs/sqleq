-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #61: sqleq-solver folded an order comparison between two strings by bytes, but s compares under en_US
-- witness: t = {(1, 'B')}: under en_US.utf8 'B' < 'a' is false, so A returns nothing and B returns 1
-- The QED prover proved it too, ordering the two constants by code point once the frontend had dropped the
-- column's collation: issue #87, which made the comparison the uninterpreted q_str_lt for en_US.utf8.
create table "t" ("id" INTEGER, "s" TEXT COLLATE "en_US.utf8");
SELECT "id" FROM "t" WHERE "s" = 'B' AND "s" < 'a';
SELECT "id" FROM "t" WHERE "s" = 'B';
