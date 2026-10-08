-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect lean: unsupported
-- origin: issue #108: the result of a function nobody declared is now a value whose = is not identity, and a cast to text over it computed in place from integers has to stay provable
-- argument: i = j makes round(i, 1) and round(j, 1) one numeric spelled one way, so the two casts are one
--   string; the two queries return the same bag
create table "t" ("id" INTEGER, "i" INTEGER, "j" INTEGER);
SELECT CAST(round("i", 1) AS TEXT) FROM "t" WHERE "i" = "j";
SELECT CAST(round("j", 1) AS TEXT) FROM "t" WHERE "i" = "j";
