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
-- origin: a qualified reference was stripped to the bare name whenever the pair used one qualifier, so with both t and s.t declared, FROM s.t read the keyed t
-- witness: s.t = {(1), (1)}: A returns 1 once and B returns it twice
create table "t" ("a" INTEGER PRIMARY KEY);
create table "s"."t" ("a" INTEGER);
SELECT DISTINCT "a" FROM "s"."t";
SELECT "a" FROM "s"."t";
