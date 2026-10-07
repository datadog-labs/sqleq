-- Unless explicitly stated otherwise all files in this repository are licensed under the
-- Apache License Version 2.0.
-- This product includes software developed at Datadog (https://www.datadoghq.com/).
-- Copyright 2026-Present Datadog, Inc.

-- truth: not-equivalent
-- expect frontend: refuse:unsupported
-- expect fuzz: not-comparable
-- expect qed: no-plan
-- expect sqleq-solver: no-plan
-- expect lean: unsupported
-- origin: issue #107: an interval column's modifier coerces a stored interval by more than its class
--   under =, and the catalog's type had dropped the modifier
-- witness: t = {(1, NULL, '1 day', '24 hours')}: iv = jv holds; A stores '1 day' in d, B stores
--   '00:00:00', because interval day keeps only the days of a value

create table "t" ("id" INTEGER PRIMARY KEY, "d" INTERVAL DAY, "iv" INTERVAL, "jv" INTERVAL);
UPDATE t SET d = iv WHERE iv = jv;
UPDATE t SET d = jv WHERE iv = jv;
