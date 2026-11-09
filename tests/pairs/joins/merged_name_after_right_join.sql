-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: an unqualified name that an outer JOIN ... USING merged must not resolve to the first
--   binding of the name: after a RIGHT join it is the COALESCE of both sides. The frontend refuses
--   it (`col_ref`), and no test reached that refusal until this pair
-- witness: t = {}, u = {(1, 1)}: the merged a is COALESCE(t.a, u.a) = 1, so A returns 1; t.a is
--   NULL, so B returns NULL
create table "t" ("a" INTEGER, "b" INTEGER);
create table "u" ("a" INTEGER, "c" INTEGER);
SELECT "a" FROM "t" RIGHT JOIN "u" USING ("a");
SELECT "t"."a" FROM "t" RIGHT JOIN "u" USING ("a");
