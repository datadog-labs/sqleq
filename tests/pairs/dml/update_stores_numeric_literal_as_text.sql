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
-- origin: issue #107: an UPDATE stored a numeric literal in a text column by its value, and 2.0 = 2.00
-- witness: t = {(1, NULL)}: A stores '2.0' in s, B stores '2.00'
-- Lowered since the fix: each side's literal is stored through q_exact_real, a function of its
-- spelling, so the two sides store two terms no prover equates.

create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT);
UPDATE t SET s = 2.0;
UPDATE t SET s = 2.00;
