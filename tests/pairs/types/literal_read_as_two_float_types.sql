-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #154: a literal compared with columns of two opaque types was one term, CAST('16777217' AS VARBINARY), so a = '16777217' AND b = '16777217' gave the provers a = b
-- witness: t = {(1, 16777216, 16777217)}: '16777217' is 16777216 as a real and 16777217 as a double precision, so the row passes both filters, A yields 1.6777216e+07 and B yields 16777217
create table "t" ("id" INTEGER PRIMARY KEY, "a" REAL, "b" DOUBLE PRECISION);
SELECT "a" FROM "t" WHERE "a" = '16777217' AND "b" = '16777217';
SELECT "b" FROM "t" WHERE "a" = '16777217' AND "b" = '16777217';
