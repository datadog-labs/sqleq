-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: no-proof
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: an index span over text, ['foo', 'foo'], on a C-collated column, where the frontend lowers <= natively; antisymmetry needs no order between
--   the constants, only an = that is identity
-- argument: under a deterministic collation such as C, s <= 'foo' and 'foo' <= s hold together exactly when s is 'foo'; both are NULL when s is NULL, as s = 'foo' is
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT COLLATE "C");
SELECT "id" FROM "t" WHERE 'foo' <= "s" AND "s" <= 'foo';
SELECT "id" FROM "t" WHERE "s" = 'foo';
