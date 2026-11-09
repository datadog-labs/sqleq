-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect sqlsolver-jvm: no-proof
-- expect cvc5: model
-- expect lean: unsupported
-- origin: the integer bound tightening of int_bound_discrete.sql is wrong for text, which sqleq-solver ranks without ordering its constants (a
--   C-collated column, so that the frontend lowers > natively)
-- witness: t = {(1, 'aa')}: 'aa' > 'a' and 'aa' < 'b' under C, so A keeps the row and B drops it
create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT COLLATE "C");
SELECT "id" FROM "t" WHERE "s" > 'a';
SELECT "id" FROM "t" WHERE "s" >= 'b';
