-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:schema
-- expect fuzz: counterexample
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: a view was not a relation to the catalog, so a bare reference to it resolved to the one
--   table of its name in another schema
-- witness: s.t = {}: FROM t reads the view, two rows of 1, so A yields 1 once and B twice
create table "s"."t" ("id" INTEGER PRIMARY KEY);
create view "t" as select 1 as "id" union all select 1;
SELECT DISTINCT "id" FROM "t";
SELECT "id" FROM "t";
