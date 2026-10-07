-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: proved
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #107: the control for the assignment cast, which is not refused where it converts by
--   value
-- argument: numeric(10,2) rounds a value stored in it by its value, and n = m on every row the two
--   statements update, so both store round(n, 2) = round(m, 2): 2.0 and 2.00 are both stored as 2.00

create table "t" ("id" INTEGER PRIMARY KEY, "p" NUMERIC(10,2), "n" NUMERIC, "m" NUMERIC);
UPDATE t SET p = n WHERE n = m;
UPDATE t SET p = m WHERE n = m;
