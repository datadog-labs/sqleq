-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: proved
-- expect cvc5: proved
-- expect lean: unsupported
-- origin: issue #166: sqleq-fuzz unescaped every `\'` in the DDL, so the `E'x\';y'` default ended
--   at its `\'` and no table was read (NO-SCHEMA)
-- argument: `>` and `<` with the operands swapped are one comparison, and the default is never read
create table "t" ("id" INTEGER PRIMARY KEY, "p" TEXT DEFAULT E'x\';y', "a" INTEGER);
SELECT "id" FROM "t" WHERE "a" > 0;
SELECT "id" FROM "t" WHERE 0 < "a";
