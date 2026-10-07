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
-- origin: issue #58: || over a numeric converts it to text, whose scale REAL does not carry
-- witness: t = {(2)}: A yields '2.0x', B yields '2.00x'
create table "t" ("x" NUMERIC);
SELECT "x" * 1.0 || 'x' FROM "t";
SELECT "x" * 1.00 || 'x' FROM "t";
