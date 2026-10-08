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
-- origin: issue #58: ->> and #>> were lowered to one symbol, though ->> looks up one key and #>> follows a path
-- witness: t = {('{"a": "x"}')}: A yields NULL (there is no key named {a}), B yields x (the path [a])
-- Refused since issue #84 (->> over a jsonb column); the two symbols stay apart on pairs that lower.
create table "t" ("j" jsonb);
SELECT "j" ->> '{a}' FROM "t";
SELECT "j" #>> '{a}' FROM "t";
