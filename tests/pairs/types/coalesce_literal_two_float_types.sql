-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect cvc5: no-proof
-- expect lean: unsupported
-- origin: issue #154: a literal argument of coalesce beside a column of an opaque type was left as written, so coalesce(a, 16777217) and coalesce(b, 16777217) returned one integer when a and b are NULL
-- witness: t = {(1, NULL, NULL)}: A yields 16777217::real, which is 1.6777216e+07, and B yields 16777217::double precision, which is 16777217
create table "t" ("id" INTEGER PRIMARY KEY, "a" REAL, "b" DOUBLE PRECISION);
SELECT coalesce("a", 16777217) FROM "t" WHERE "a" IS NULL AND "b" IS NULL;
SELECT coalesce("b", 16777217) FROM "t" WHERE "a" IS NULL AND "b" IS NULL;
