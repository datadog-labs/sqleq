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
-- origin: issue #58: jsonb_extract_path_text was lowered to the symbol of ->>, though it follows a path and ->> looks up a key
-- witness: t = {('[5]')}: A yields 5 (the path [0] reaches the first element), B yields NULL (an array has no key named 0)
-- Refused since issue #84 (->> over a jsonb column); the two symbols stay apart on pairs that lower.
create table "t" ("j" jsonb);
SELECT jsonb_extract_path_text("j", '0') FROM "t";
SELECT "j" ->> '0' FROM "t";
