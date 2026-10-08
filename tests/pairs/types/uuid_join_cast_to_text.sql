-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #108: identity became the case to establish, and uuid, whose = is identity, has to keep the proofs it had
-- argument: uuid's = compares the 128 bits, and its text is their canonical spelling, so a = b makes the
--   two casts one string; the two queries return the same bag
create table "t" ("id" INTEGER, "a" uuid, "b" uuid);
SELECT CAST("a" AS TEXT) FROM "t" WHERE "a" = "b";
SELECT CAST("b" AS TEXT) FROM "t" WHERE "a" = "b";
