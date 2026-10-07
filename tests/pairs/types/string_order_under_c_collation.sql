-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #87: under COLLATE "C" strings are ordered by code point, as the provers order them, so the comparison stays native and the QED prover decides it
-- argument: under C 'B' < 'a' (0x42 < 0x61), so s = 'B' implies s < 'a' and the second conjunct is redundant
create table "t" ("id" INTEGER, "s" TEXT COLLATE "C");
SELECT "id" FROM "t" WHERE "s" = 'B' AND "s" < 'a';
SELECT "id" FROM "t" WHERE "s" = 'B';
