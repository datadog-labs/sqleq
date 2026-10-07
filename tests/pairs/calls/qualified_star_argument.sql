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
-- origin: issue #52: a t.* function argument was dropped, so both calls were nullary
-- witness: t = {(1, 10)}, u = {(1, 20)}: A yields {"id":1,"a":10}, B yields {"id":1,"b":20}
create table "t" ("id" INTEGER, "a" INTEGER);
create table "u" ("id" INTEGER, "b" INTEGER);
SELECT row_to_json("t".*) FROM "t" JOIN "u" ON "t"."id" = "u"."id";
SELECT row_to_json("u".*) FROM "t" JOIN "u" ON "t"."id" = "u"."id";
