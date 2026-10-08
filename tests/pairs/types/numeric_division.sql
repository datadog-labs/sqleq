-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #58: numeric division was read as exact division
-- witness: t = {(1)}: A yields 0.99999999999999999990 (numeric division rounds to a finite scale), B yields 1
-- Refused since issue #84: the scale of a numeric quotient follows its operands' scales, so x / 3.0 is
-- not a function of x's value.
create table "t" ("x" NUMERIC);
SELECT "x" / 3.0 * 3.0 FROM "t";
SELECT "x" FROM "t";
