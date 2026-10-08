-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: no-counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #107: an UPDATE stored a numeric in a text column by its class under =, though the
--   assignment cast writes its spelling
-- witness: t = {(1, NULL, 2.0, 2.00)}: n = m holds; A stores '2.0' in s, B stores '2.00'

create table "t" ("id" INTEGER PRIMARY KEY, "s" TEXT, "n" NUMERIC, "m" NUMERIC);
UPDATE t SET s = n WHERE n = m;
UPDATE t SET s = m WHERE n = m;
