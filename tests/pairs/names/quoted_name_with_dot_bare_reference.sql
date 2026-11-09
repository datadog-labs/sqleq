-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect cvc5: no-plan
-- expect lean: unsupported
-- origin: issue #203: a bare name resolved to the one declared name ending in ".t", which the
--   one-part "s.t" does, so FROM t read "s.t", key and all
-- witness: "s.t" = {}, t = {(1, 1), (1, 2)}: A returns 1 once, B twice
create table "s.t" ("a" INTEGER PRIMARY KEY, "b" INTEGER);
SELECT DISTINCT "a" FROM "t";
SELECT "a" FROM "t";
