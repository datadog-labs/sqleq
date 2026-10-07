-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: not-comparable
-- expect qed: no-proof
-- expect sqleq-solver: no-proof
-- expect lean: unsupported
-- origin: issue #58: numeric division was read as exact division
-- witness: t = {(1)}: A yields 0.99999999999999999990 (numeric division rounds to a finite scale), B yields 1
create table "t" ("x" NUMERIC);
SELECT "x" / 3.0 * 3.0 FROM "t";
SELECT "x" FROM "t";
