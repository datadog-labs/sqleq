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
-- origin: issue #154: the control for literal_read_as_two_float_types, whose fix reads a literal at each column's declared type: two columns of one type read it as one value, so the proof stays
-- argument: '1.5' is one double precision value, so a = '1.5' AND b = '1.5' makes a and b equal, and the two queries return the same bag
create table "t" ("id" INTEGER PRIMARY KEY, "a" DOUBLE PRECISION, "b" DOUBLE PRECISION);
SELECT "a" FROM "t" WHERE "a" = '1.5' AND "b" = '1.5';
SELECT "b" FROM "t" WHERE "a" = '1.5' AND "b" = '1.5';
