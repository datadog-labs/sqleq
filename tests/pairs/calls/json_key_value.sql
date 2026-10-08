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
-- origin: issue #52: SQL/JSON key/value arguments were dropped, so both calls were nullary
-- witness: t = {(1, 2)}: A yields {"k" : 1}, B yields {"j" : 2}
create table "t" ("a" INTEGER, "b" INTEGER);
SELECT json_object('k' VALUE "a") FROM "t";
SELECT json_object('j' VALUE "b") FROM "t";
