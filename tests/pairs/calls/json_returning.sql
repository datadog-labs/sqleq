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
-- origin: issue #52: the SQL/JSON RETURNING clause was dropped
-- witness: t = {('{"b":1,"a":2}')}: A yields [{"b":1,"a":2}], B yields [{"a": 2, "b": 1}]
create table "t" ("j" json);
SELECT CAST(json_array("j" RETURNING json) AS text) FROM "t";
SELECT CAST(json_array("j" RETURNING jsonb) AS text) FROM "t";
