-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: error
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #52: the SQL/JSON ABSENT ON NULL / NULL ON NULL clause was dropped
-- witness: t = {(NULL)}: A yields [], B yields [null]
create table "t" ("a" INTEGER);
SELECT json_array("a" ABSENT ON NULL) FROM "t";
SELECT json_array("a" NULL ON NULL) FROM "t";
