-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect sqlsolver-jvm: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #164: collation::BLIND spelled the JSON symbols in lower case, so a key lookup was
--   refused beside any column that declares a collation
-- argument: `j -> 'k'` yields jsonb, which has no collation, and looks the key up byte-wise; the two
--   WHERE conditions are the same comparison written both ways round
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT COLLATE "en_US", "j" JSONB);
SELECT "j" -> 'k' FROM "t" WHERE "id" = 1;
SELECT "j" -> 'k' FROM "t" WHERE 1 = "id";
