-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #107: an INSERT ... VALUES stored a numeric literal in a text column by its value, and
--   2.0 = 2.00
-- witness: t = {}: A inserts '2.0', B inserts '2.00'

create table "t" ("s" TEXT);
INSERT INTO t (s) VALUES (2.0);
INSERT INTO t (s) VALUES (2.00);
