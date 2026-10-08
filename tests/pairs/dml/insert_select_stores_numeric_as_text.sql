-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #107: an INSERT ... SELECT stored a numeric in a text column by its class under =,
--   though the assignment cast writes its spelling
-- witness: u = {(2.0, 2.00)}: A inserts '2.0', B inserts '2.00'

create table "t" ("s" TEXT);
create table "u" ("n" NUMERIC, "m" NUMERIC);
INSERT INTO t (s) SELECT n FROM u WHERE n = m;
INSERT INTO t (s) SELECT m FROM u WHERE n = m;
