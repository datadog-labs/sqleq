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
-- origin: issue #92 item 5: a literal path under #> is read into its elements, and only the
--   operator's literal is: the call's '{a}' is one key spelled with braces
-- witness: t = {(1, '{"{a}": 1, "a": 2}')}: A yields 2 (the path [a]), B yields 1 (the key {a})
create table "t" ("id" INTEGER PRIMARY KEY, "j" JSONB);
SELECT "id", "j" #> '{a}' AS "v" FROM "t";
SELECT "id", jsonb_extract_path("j", '{a}') AS "v" FROM "t";
