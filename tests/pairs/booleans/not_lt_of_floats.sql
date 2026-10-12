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
-- origin: issue #92 item 1 (control): a float is opaque, and a NOT over a comparison of opaque values is not put through, so the pair stays unproved
-- argument: >= is the negator of < on double precision, NaN included, which Postgres orders above every other float and calls equal to itself: when f is NULL both sides are NULL, and otherwise one is true exactly when the other is false
create table "t" ("id" INTEGER PRIMARY KEY, "f" DOUBLE PRECISION);
SELECT "id" FROM "t" WHERE NOT ("f" < 1);
SELECT "id" FROM "t" WHERE "f" >= 1;
