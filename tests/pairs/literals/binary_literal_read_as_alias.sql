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
-- origin: issue #80: sqlparser split the binary literal 0b101 into the number 0 and the alias
--   b101, so the two queries lowered to one plan
-- witness: t = {(1)}: A yields 5, B yields 0
create table "t" ("id" INTEGER, unique ("id"));
SELECT 0b101 FROM "t";
SELECT 0 FROM "t";
