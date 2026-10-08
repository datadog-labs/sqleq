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
-- origin: issue #58: char(n) was mapped to VARCHAR, whose equality counts trailing spaces; a char(n) value is now refused
-- witness: t = {('a')} (char(3)): A yields no rows (char(n) comparison ignores trailing spaces), B yields ('a  ')
create table "t" ("c" char(3));
SELECT "c" FROM "t" WHERE "c" = 'a' AND NOT "c" = 'a ';
SELECT "c" FROM "t" WHERE "c" = 'a';
