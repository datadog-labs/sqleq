-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #58: int4range was mapped to INTEGER, because its name contains INT
-- witness: t = {('[1,3)')}: A yields empty (the union of r with itself, minus r), B yields [1,3)
create table "t" ("r" int4range);
SELECT ("r" + "r") - "r" FROM "t";
SELECT "r" FROM "t";
