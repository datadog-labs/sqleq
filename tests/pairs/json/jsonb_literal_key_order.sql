-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: equivalent
-- expect frontend: emit
-- expect fuzz: no-counterexample
-- expect qed: no-proof
-- expect sqleq-solver: unsupported
-- expect lean: unsupported
-- origin: issue #89: sqleq-fuzz compared a jsonb literal with the column as text, so a key order
--   other than the generated documents' one never matched
-- argument: the two literals are one jsonb value (jsonb does not keep key order). Postgres 17 returns
--   id 0 for both on t = {(0, '{"a":1,"b":"b"}'), (1, '{"b":2,"c":"c"}')}

create table "t" ("id" INTEGER, "j" JSONB, unique ("id"));
SELECT "id" FROM "t" WHERE "j" = '{"a":1,"b":"b"}';
SELECT "id" FROM "t" WHERE "j" = '{"b":"b","a":1}';
