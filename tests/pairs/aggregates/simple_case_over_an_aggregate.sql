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
-- origin: a simple CASE over an aggregate is refused (`lower_post`); with its operand dropped, both
--   sides would lower to CASE WHEN 1. No test reached that refusal until this pair
-- witness: t = {(1, 5)}: count(*) is 1, so A returns 1; sum(b) is 5, so B returns 2
create table "t" ("id" INTEGER, "b" INTEGER, unique ("id"));
SELECT CASE count(*) WHEN 1 THEN 1 ELSE 2 END FROM "t";
SELECT CASE sum("b") WHEN 1 THEN 1 ELSE 2 END FROM "t";
