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
-- origin: a pair file kept a column's declared type as its domain's name, so an assignment to a domain over interval day was not seen to truncate, where raw DDL resolved the domain and refused
-- witness: t = {(1, NULL, '24 hours')}: '24 hours' = '1 day' holds, and A stores it truncated to the day field, 00:00:00, where B stores 1 day
create domain "d" as interval day;
create table "t" ("id" INTEGER PRIMARY KEY, "x" "d", "y" INTERVAL);
UPDATE "t" SET "x" = "y" WHERE "y" = INTERVAL '1 day';
UPDATE "t" SET "x" = INTERVAL '1 day' WHERE "y" = INTERVAL '1 day';
